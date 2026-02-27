mod audit_event_sink;
mod auto_settlement;
mod chain_callback_sink;
mod data_layer_consistency;
mod integration_flows;
mod outbox_publisher;
mod room_service_port;
mod rpc_reject_audit_sink;
mod rpc_request_signature_verifier;
mod seat_crypto_audit_sink;
mod settlement_audit_sink;
mod settlement_orchestrator;
mod showdown_admin_repo;
mod showdown_settlement;

use agent_auth::InMemoryReplayNonceStore;
use anyhow::Result;
use audit_event_sink::AuditRoomEventSink;
use audit_store::{
    AuditReadRepository, AuditRepository, InMemoryAuditRepository, NoopAuditRepository,
    PostgresAuditRepository,
};
use auto_settlement::{
    AppRoomShowdownInputProvider, AutoSettlementLoopConfig, spawn_auto_settlement_loop,
    spawn_auto_settlement_loop_with_batch,
};
use ledger_store::{
    ChainTxRepository, LedgerRepository, NoopChainTxRepository, NoopLedgerRepository,
    PostgresChainTxRepository, PostgresLedgerRepository,
};
use jsonrpsee::server::ServerBuilder;
use observability::init_tracing;
use ops_http::{InMemoryRoomAdminReadRepository, OpsState, PostgresRoomAdminReadRepository};
use platform_core::AppConfig;
use poker_domain::RoomId;
use poker_engine::PokerEngine;
use room_service_port::AppRoomService;
use rpc_gateway::{InMemoryEventBus, RpcGateway, RpcGatewayLimits};
use rpc_reject_audit_sink::AuditRpcRejectSink;
use rpc_request_signature_verifier::AppRpcRequestSignatureVerifier;
use settlement::{
    BatchSettlementWalletAdapter, ReqwestEvmBatchSettlementWalletAdapter,
    ReqwestEvmSettlementWalletAdapter, SettlementWalletAdapter,
};
use showdown_admin_repo::AppShowdownAdminRepository;
use sqlx::postgres::PgPoolOptions;
use std::{env, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tracing::info;

struct RepoBundle {
    ledger_repo: Arc<dyn LedgerRepository>,
    audit_repo: Arc<dyn AuditRepository>,
    chain_tx_repo: Arc<dyn ChainTxRepository>,
    audit_read_repo: Arc<dyn AuditReadRepository>,
    chain_tx_read_repo: Arc<dyn ledger_store::ChainTxReadRepository>,
    room_admin_read_repo: Arc<dyn ops_http::RoomAdminReadRepository>,
    settlement_read_repo: Arc<dyn ledger_store::SettlementPersistenceReadRepository>,
    settlement_write_repo: Arc<dyn ledger_store::SettlementPersistenceRepository>,
    db_pool: Option<sqlx::PgPool>,
    backend: &'static str,
}

#[derive(Clone)]
struct DynLedgerRepo(Arc<dyn LedgerRepository>);

#[async_trait::async_trait]
impl LedgerRepository for DynLedgerRepo {
    async fn insert_entries(
        &self,
        entries: &[ledger_store::LedgerEntryInsert],
    ) -> Result<(), ledger_store::LedgerStoreError> {
        self.0.insert_entries(entries).await
    }
}

#[derive(Clone)]
struct DynSettlementRepo(Arc<dyn ledger_store::SettlementPersistenceRepository>);

#[async_trait::async_trait]
impl ledger_store::SettlementPersistenceRepository for DynSettlementRepo {
    async fn insert_settlement_plan(
        &self,
        record: &ledger_store::SettlementPlanRecordInsert,
    ) -> Result<(), ledger_store::LedgerStoreError> {
        self.0.insert_settlement_plan(record).await
    }

    async fn insert_settlement_record(
        &self,
        record: &ledger_store::SettlementRecordInsert,
    ) -> Result<(), ledger_store::LedgerStoreError> {
        self.0.insert_settlement_record(record).await
    }

    async fn update_settlement_record_status_by_tx_hash(
        &self,
        update: &ledger_store::SettlementRecordStatusUpdate,
    ) -> Result<(), ledger_store::LedgerStoreError> {
        self.0
            .update_settlement_record_status_by_tx_hash(update)
            .await
    }

    async fn upsert_settlement_showdown_input(
        &self,
        record: &ledger_store::SettlementShowdownInputRecord,
    ) -> Result<(), ledger_store::LedgerStoreError> {
        self.0.upsert_settlement_showdown_input(record).await
    }
}

#[derive(Clone)]
struct DynWallet(Arc<dyn SettlementWalletAdapter>);

#[async_trait::async_trait]
impl SettlementWalletAdapter for DynWallet {
    async fn submit_transfer(
        &self,
        request: &settlement::SettlementTransferRequest,
    ) -> Result<String, String> {
        self.0.submit_transfer(request).await
    }

    async fn get_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<settlement::SettlementTransferReceipt, String> {
        self.0.get_transfer_receipt(tx_hash).await
    }
}

#[derive(Clone)]
struct DynBatchWallet(Arc<dyn BatchSettlementWalletAdapter>);

#[async_trait::async_trait]
impl BatchSettlementWalletAdapter for DynBatchWallet {
    async fn submit_batch_transfer(
        &self,
        request: &settlement::BatchSettlementTransferRequest,
    ) -> Result<String, String> {
        self.0.submit_batch_transfer(request).await
    }

    async fn get_batch_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<settlement::BatchSettlementTransferReceipt, String> {
        self.0.get_batch_transfer_receipt(tx_hash).await
    }
}

fn build_repo_bundle() -> Result<RepoBundle> {
    let database_url = match env::var("DATABASE_URL") {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    };

    if let Some(url) = database_url {
        let max_connections = env::var("POSTGRES_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(20);
        let min_connections = env::var("POSTGRES_MIN_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(2)
            .min(max_connections);
        let acquire_timeout_ms = env::var("POSTGRES_ACQUIRE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(10_000);
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .min_connections(min_connections)
            .acquire_timeout(std::time::Duration::from_millis(acquire_timeout_ms))
            .connect_lazy(&url)?;
        info!(
            postgres_max_connections = max_connections,
            postgres_min_connections = min_connections,
            postgres_acquire_timeout_ms = acquire_timeout_ms,
            "postgres pool configured"
        );
        let audit_pg = Arc::new(PostgresAuditRepository::new(pool.clone()));
        let chain_pg = Arc::new(PostgresChainTxRepository::new(pool.clone()));
        let ledger_pg = Arc::new(PostgresLedgerRepository::new(pool.clone()));
        let audit_repo: Arc<dyn AuditRepository> = audit_pg.clone();
        let audit_read_repo: Arc<dyn AuditReadRepository> = audit_pg;
        let chain_tx_repo: Arc<dyn ChainTxRepository> = chain_pg.clone();
        let chain_tx_read_repo: Arc<dyn ledger_store::ChainTxReadRepository> = chain_pg.clone();
        let settlement_read_repo: Arc<dyn ledger_store::SettlementPersistenceReadRepository> =
            chain_pg.clone();
        let settlement_write_repo: Arc<dyn ledger_store::SettlementPersistenceRepository> =
            chain_pg;
        let room_admin_read_repo: Arc<dyn ops_http::RoomAdminReadRepository> =
            Arc::new(PostgresRoomAdminReadRepository::new(pool.clone()));
        return Ok(RepoBundle {
            ledger_repo: ledger_pg,
            audit_repo,
            chain_tx_repo,
            audit_read_repo,
            chain_tx_read_repo,
            room_admin_read_repo,
            settlement_read_repo,
            settlement_write_repo,
            db_pool: Some(pool),
            backend: "postgres",
        });
    }

    let audit_mem = Arc::new(InMemoryAuditRepository::default());
    let chain_mem = Arc::new(ledger_store::InMemoryChainTxRepository::new());
    let settlement_mem = Arc::new(ledger_store::InMemorySettlementPersistenceRepository::new());
    Ok(RepoBundle {
        ledger_repo: Arc::new(NoopLedgerRepository),
        audit_repo: Arc::new(NoopAuditRepository),
        chain_tx_repo: Arc::new(NoopChainTxRepository),
        audit_read_repo: audit_mem,
        chain_tx_read_repo: chain_mem,
        room_admin_read_repo: Arc::new(InMemoryRoomAdminReadRepository::new()),
        settlement_read_repo: settlement_mem.clone(),
        settlement_write_repo: settlement_mem,
        db_pool: None,
        backend: "noop",
    })
}

fn rpc_gateway_limits_from_env() -> RpcGatewayLimits {
    let mut limits = RpcGatewayLimits::default();
    if let Ok(v) = env::var("RPC_MAX_CONCURRENT_PER_CONNECTION")
        && let Ok(parsed) = v.parse::<usize>()
    {
        limits.max_concurrent_per_connection = parsed.max(1);
    }
    if let Ok(v) = env::var("RPC_MAX_RPS_PER_CONNECTION")
        && let Ok(parsed) = v.parse::<usize>()
    {
        limits.max_requests_per_second_per_connection = parsed.max(1);
    }
    limits
}

fn ops_http_enabled_from_env() -> bool {
    match env::var("APP_SERVER__OPS_HTTP_ENABLED") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

fn auto_settlement_enabled_from_env() -> bool {
    match env::var("APP_SERVER__AUTO_SETTLEMENT_ENABLED") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

fn build_settlement_wallet_adapter_from_env() -> Option<Arc<dyn SettlementWalletAdapter>> {
    let endpoint = env::var("SETTLEMENT_EVM_RPC_URL").ok()?;
    let from_address = env::var("SETTLEMENT_FROM_ADDRESS").ok()?;
    if endpoint.trim().is_empty() || from_address.trim().is_empty() {
        return None;
    }
    Some(Arc::new(ReqwestEvmSettlementWalletAdapter::new(
        endpoint,
        from_address,
    )))
}

fn build_batch_settlement_wallet_adapter_from_env() -> Option<Arc<dyn BatchSettlementWalletAdapter>>
{
    let endpoint = env::var("SETTLEMENT_EVM_RPC_URL").ok()?;
    let from_address = env::var("SETTLEMENT_FROM_ADDRESS").ok()?;
    let contract = env::var("SETTLEMENT_BATCH_CONTRACT_ADDRESS").ok()?;
    if endpoint.trim().is_empty() || from_address.trim().is_empty() || contract.trim().is_empty() {
        return None;
    }
    Some(Arc::new(ReqwestEvmBatchSettlementWalletAdapter::new(
        endpoint,
        from_address,
        contract,
    )))
}

async fn spawn_ops_http_server(
    bind_addr: &str,
    router: axum::Router,
) -> Result<tokio::task::JoinHandle<()>> {
    let listener = TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            tracing::warn!(error = %err, "ops-http server exited");
        }
    });
    info!(ops_http_addr = %local_addr, "ops-http server started");
    Ok(handle)
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = AppConfig::load()?;
    init_tracing(&config.app.service_name);
    info!(
        env = config.app.env.as_str(),
        rpc_bind_addr = %config.app.rpc_bind_addr,
        ops_http_bind_addr = %config.app.ops_http_bind_addr,
        log_filter = %config.observability.log_filter,
        "app config loaded"
    );

    let _engine = PokerEngine::new();
    let gateway = RpcGateway::new();
    gateway.log_bootstrap();
    let repo_bundle = build_repo_bundle()?;
    info!(
        storage_backend = repo_bundle.backend,
        db_enabled = repo_bundle.db_pool.is_some(),
        "storage repositories initialized"
    );
    let audit_repo = repo_bundle.audit_repo.clone();
    let chain_tx_repo = repo_bundle.chain_tx_repo.clone();
    let room_event_sink = Arc::new(AuditRoomEventSink::new(audit_repo.clone()));
    let room_service = AppRoomService::with_sinks(room_event_sink, chain_tx_repo.clone());
    let event_bus = Arc::new(InMemoryEventBus::new());
    let rpc_reject_audit_sink = Arc::new(AuditRpcRejectSink::new(repo_bundle.audit_repo.clone()));
    let rpc_request_signature_verifier =
        Arc::new(AppRpcRequestSignatureVerifier::new(room_service.clone()));
    let showdown_provider = Arc::new(
        AppRoomShowdownInputProvider::new(room_service.clone()).with_persistence(
            repo_bundle.settlement_read_repo.clone(),
            repo_bundle.settlement_write_repo.clone(),
        ),
    );
    let ops_router = ops_http::build_router_with_state(OpsState {
        audit_read: repo_bundle.audit_read_repo.clone(),
        chain_tx_read: repo_bundle.chain_tx_read_repo.clone(),
        room_admin_read: repo_bundle.room_admin_read_repo.clone(),
        settlement_read: repo_bundle.settlement_read_repo.clone(),
        settlement_write: repo_bundle.settlement_write_repo.clone(),
        showdown_admin: Arc::new(AppShowdownAdminRepository::new(
            (*showdown_provider).clone(),
        )),
    });
    let _ops_http_handle = if ops_http_enabled_from_env() {
        Some(spawn_ops_http_server(&config.app.ops_http_bind_addr, ops_router).await?)
    } else {
        info!("ops-http server disabled by APP_SERVER__OPS_HTTP_ENABLED");
        None
    };
    let (_outbox_shutdown_tx, outbox_shutdown_rx) = oneshot::channel();
    let _outbox_handle = outbox_publisher::spawn_outbox_publisher_loop(
        audit_repo,
        event_bus.clone(),
        Duration::from_millis(200),
        100,
        outbox_shutdown_rx,
    );
    let rpc_limits = rpc_gateway_limits_from_env();
    info!(
        rpc_max_concurrent_per_connection = rpc_limits.max_concurrent_per_connection,
        rpc_max_rps_per_connection = rpc_limits.max_requests_per_second_per_connection,
        "rpc gateway limits configured"
    );
    let rpc_module = gateway.build_rpc_module_with_native_pubsub_and_limits_and_audit(
        Arc::new(room_service.clone()),
        event_bus,
        rpc_limits,
        Some(rpc_reject_audit_sink),
        Some(Arc::new(InMemoryReplayNonceStore::new())),
        Some(rpc_request_signature_verifier),
    )?;
    let rpc_method_count = rpc_module.method_names().count();
    let rpc_server = ServerBuilder::default()
        .build(&config.app.rpc_bind_addr)
        .await?;
    let rpc_local_addr = rpc_server.local_addr()?;
    let rpc_handle = rpc_server.start(rpc_module);
    info!(rpc_addr = %rpc_local_addr, "json-rpc server started");
    let _settlement_wallet_adapter = build_settlement_wallet_adapter_from_env();
    let _batch_settlement_wallet_adapter = build_batch_settlement_wallet_adapter_from_env();
    let settlement_mode = if _batch_settlement_wallet_adapter.is_some() {
        "batch"
    } else if _settlement_wallet_adapter.is_some() {
        "direct"
    } else {
        "disabled"
    };
    info!(
        settlement_wallet_enabled = _settlement_wallet_adapter.is_some(),
        batch_settlement_wallet_enabled = _batch_settlement_wallet_adapter.is_some(),
        settlement_mode,
        "settlement wallet adapter configured"
    );
    let (_auto_settlement_shutdown_tx, _auto_settlement_handle) =
        if auto_settlement_enabled_from_env() {
            if let Some(batch_wallet) = _batch_settlement_wallet_adapter.clone() {
                let (auto_tx, auto_rx) = oneshot::channel();
                let room_service_for_loop = room_service.clone();
                let room_ids_provider: Arc<dyn Fn() -> Result<Vec<RoomId>, String> + Send + Sync> =
                    Arc::new(move || room_service_for_loop.room_ids().map_err(|e| e.to_string()));
                let settlement_service = Arc::new(settlement::SettlementService::new(
                    DynLedgerRepo(repo_bundle.ledger_repo.clone()),
                ));
                let settlement_repo =
                    Arc::new(DynSettlementRepo(repo_bundle.settlement_write_repo.clone()));
                let audit_sink = Arc::new(settlement_audit_sink::AuditSettlementBehaviorSink::new(
                    repo_bundle.audit_repo.clone(),
                ));
                let handle = spawn_auto_settlement_loop_with_batch(
                    room_service.clone(),
                    room_ids_provider,
                    Arc::new(PokerEngine::new()),
                    settlement_service,
                    settlement_repo,
                    Arc::new(DynBatchWallet(batch_wallet)),
                    showdown_provider.clone(),
                    AutoSettlementLoopConfig::default(),
                    auto_rx,
                    None,
                    Some(audit_sink),
                );
                info!("auto settlement loop enabled (batch payout mode)");
                (Some(auto_tx), Some(handle))
            } else if let Some(wallet) = _settlement_wallet_adapter.clone() {
                let (auto_tx, auto_rx) = oneshot::channel();
                let room_service_for_loop = room_service.clone();
                let room_ids_provider: Arc<dyn Fn() -> Result<Vec<RoomId>, String> + Send + Sync> =
                    Arc::new(move || room_service_for_loop.room_ids().map_err(|e| e.to_string()));
                let settlement_service = Arc::new(settlement::SettlementService::new(
                    DynLedgerRepo(repo_bundle.ledger_repo.clone()),
                ));
                let settlement_repo =
                    Arc::new(DynSettlementRepo(repo_bundle.settlement_write_repo.clone()));
                let audit_sink = Arc::new(settlement_audit_sink::AuditSettlementBehaviorSink::new(
                    repo_bundle.audit_repo.clone(),
                ));
                let handle = spawn_auto_settlement_loop(
                    room_service.clone(),
                    room_ids_provider,
                    Arc::new(PokerEngine::new()),
                    settlement_service,
                    settlement_repo,
                    Arc::new(DynWallet(wallet)),
                    showdown_provider.clone(),
                    AutoSettlementLoopConfig::default(),
                    auto_rx,
                    None,
                    Some(audit_sink),
                );
                info!("auto settlement loop enabled (direct payout mode)");
                (Some(auto_tx), Some(handle))
            } else {
                info!("auto settlement loop not started: wallet adapter disabled");
                (None, None)
            }
        } else {
            info!("auto settlement loop disabled by APP_SERVER__AUTO_SETTLEMENT_ENABLED");
            (None, None)
        };
    info!(rpc_methods = rpc_method_count, "rpc module ready");
    info!("app-server bootstrap complete");
    rpc_handle.stopped().await;
    info!("json-rpc server stopped");
    Ok(())
}
