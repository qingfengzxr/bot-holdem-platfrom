use async_trait::async_trait;
use poker_domain::{Chips, HandId, RoomId, SeatId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::debug;

#[derive(Debug, Error)]
pub enum ChainWatcherError {
    #[error("unsupported chain rpc")]
    UnsupportedRpc,
    #[error("callback sink error: {0}")]
    CallbackSink(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("rpc response error: {0}")]
    Rpc(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationStatus {
    Pending,
    Matched,
    Late,
    Unmatched,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVerificationInput {
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<u32>,
    pub tx_hash: String,
    pub expected_to: String,
    pub expected_from: Option<String>,
    pub expected_amount: Option<Chips>,
    pub min_confirmations: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVerificationResult {
    pub tx_hash: String,
    pub status: VerificationStatus,
    pub confirmations: u64,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxVerificationCallback {
    pub room_id: RoomId,
    pub hand_id: Option<HandId>,
    pub seat_id: Option<SeatId>,
    pub action_seq: Option<u32>,
    pub tx_hash: String,
    pub status: VerificationStatus,
    pub confirmations: u64,
    pub failure_reason: Option<String>,
}

#[async_trait]
pub trait VerificationCallbackSink: Send + Sync {
    async fn on_verification_result(
        &self,
        callback: TxVerificationCallback,
    ) -> Result<(), ChainWatcherError>;
}

#[async_trait]
pub trait EvmLikeTxRpcClient: Send + Sync {
    // Target is Conflux eSpace (EVM-compatible). Local/dev can use Foundry Anvil.
    async fn get_tx(&self, tx_hash: &str) -> Result<Option<ObservedTx>, ChainWatcherError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestEvmJsonRpcClient {
    endpoint: String,
    client: reqwest::Client,
}

impl ReqwestEvmJsonRpcClient {
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }

    #[must_use]
    pub fn with_client(endpoint: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            endpoint: endpoint.into(),
            client,
        }
    }

    async fn rpc_call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, ChainWatcherError> {
        let body = JsonRpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method,
            params,
        };
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| ChainWatcherError::Http(e.to_string()))?;
        let payload: JsonRpcResponse<T> = resp
            .json()
            .await
            .map_err(|e| ChainWatcherError::Http(e.to_string()))?;
        if let Some(err) = payload.error {
            return Err(ChainWatcherError::Rpc(format!(
                "code={} message={}",
                err.code, err.message
            )));
        }
        payload
            .result
            .ok_or_else(|| ChainWatcherError::Rpc("missing result".to_string()))
    }
}

#[async_trait]
impl EvmLikeTxRpcClient for ReqwestEvmJsonRpcClient {
    async fn get_tx(&self, tx_hash: &str) -> Result<Option<ObservedTx>, ChainWatcherError> {
        let tx: Option<EthTransaction> = self
            .rpc_call("eth_getTransactionByHash", serde_json::json!([tx_hash]))
            .await?;
        let Some(tx) = tx else {
            return Ok(None);
        };

        let receipt: Option<EthTransactionReceipt> = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        let latest_block_hex: String = self
            .rpc_call("eth_blockNumber", serde_json::json!([]))
            .await?;

        let latest_block = parse_hex_u64(&latest_block_hex)?;
        let tx_block = tx
            .block_number
            .as_deref()
            .map(parse_hex_u64)
            .transpose()?
            .unwrap_or(0);

        let confirmations = if tx_block == 0 || latest_block < tx_block {
            0
        } else {
            latest_block.saturating_sub(tx_block).saturating_add(1)
        };

        let success = receipt
            .as_ref()
            .and_then(|r| r.status.as_deref())
            .map(parse_hex_u64)
            .transpose()?
            .map(|v| v == 1)
            .unwrap_or(true);

        Ok(Some(ObservedTx {
            tx_hash: tx.hash,
            from: tx.from,
            to: tx.to.unwrap_or_default(),
            value: Chips(parse_hex_u128(&tx.value)?),
            success,
            confirmations,
        }))
    }
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    params: Value,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Value,
    result: Option<T>,
    error: Option<JsonRpcErrorObj>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcErrorObj {
    code: i64,
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
struct EthTransaction {
    hash: String,
    from: String,
    to: Option<String>,
    value: String,
    #[serde(rename = "blockNumber")]
    block_number: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct EthTransactionReceipt {
    status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedTx {
    pub tx_hash: String,
    pub from: String,
    pub to: String,
    pub value: Chips,
    pub success: bool,
    pub confirmations: u64,
}

#[derive(Debug, Default)]
pub struct ChainWatcher;

impl ChainWatcher {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub async fn verify_tx(
        &self,
        input: TxVerificationInput,
    ) -> Result<TxVerificationResult, ChainWatcherError> {
        debug!(tx_hash = %input.tx_hash, "chain watcher placeholder verify");
        Ok(TxVerificationResult {
            tx_hash: input.tx_hash,
            status: VerificationStatus::Pending,
            confirmations: 0,
            failure_reason: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct EvmChainWatcher<C> {
    client: C,
    retry_policy: RetryPolicy,
}

impl<C> EvmChainWatcher<C> {
    #[must_use]
    pub fn new(client: C, retry_policy: RetryPolicy) -> Self {
        Self {
            client,
            retry_policy,
        }
    }

    #[must_use]
    pub fn retry_policy(&self) -> RetryPolicy {
        self.retry_policy
    }
}

impl<C> EvmChainWatcher<C>
where
    C: EvmLikeTxRpcClient,
{
    pub async fn verify_tx(
        &self,
        input: &TxVerificationInput,
    ) -> Result<TxVerificationResult, ChainWatcherError> {
        verify_tx_with_retry(&self.client, input, self.retry_policy).await
    }

    pub async fn verify_and_dispatch<S: VerificationCallbackSink>(
        &self,
        input: &TxVerificationInput,
        sink: &S,
    ) -> Result<TxVerificationResult, ChainWatcherError> {
        let result = self.verify_tx(input).await?;
        dispatch_verification_callback(sink, input, &result).await?;
        Ok(result)
    }
}

fn parse_hex_u64(input: &str) -> Result<u64, ChainWatcherError> {
    let s = input.trim();
    let raw = s.strip_prefix("0x").unwrap_or(s);
    if raw.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(raw, 16).map_err(|e| ChainWatcherError::Parse(e.to_string()))
}

fn parse_hex_u128(input: &str) -> Result<u128, ChainWatcherError> {
    let s = input.trim();
    let raw = s.strip_prefix("0x").unwrap_or(s);
    if raw.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(raw, 16).map_err(|e| ChainWatcherError::Parse(e.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_backoff_ms: 300,
            max_backoff_ms: 5_000,
        }
    }
}

impl RetryPolicy {
    #[must_use]
    pub fn backoff_ms(&self, attempt_no: u32) -> u64 {
        let factor = 2_u64.saturating_pow(attempt_no.min(10));
        self.base_backoff_ms
            .saturating_mul(factor)
            .min(self.max_backoff_ms)
    }
}

pub async fn query_tx_with_retry<C: EvmLikeTxRpcClient>(
    client: &C,
    tx_hash: &str,
    retry: RetryPolicy,
) -> Result<Option<ObservedTx>, ChainWatcherError> {
    for attempt in 0..retry.max_attempts {
        if let Some(tx) = client.get_tx(tx_hash).await? {
            return Ok(Some(tx));
        }
        if attempt + 1 < retry.max_attempts {
            tokio::time::sleep(std::time::Duration::from_millis(retry.backoff_ms(attempt))).await;
        }
    }
    Ok(None)
}

pub async fn verify_tx_with_retry<C: EvmLikeTxRpcClient>(
    client: &C,
    input: &TxVerificationInput,
    retry: RetryPolicy,
) -> Result<TxVerificationResult, ChainWatcherError> {
    let observed = query_tx_with_retry(client, &input.tx_hash, retry).await?;
    let Some(observed) = observed else {
        return Ok(TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Pending,
            confirmations: 0,
            failure_reason: Some("tx_not_found".to_string()),
        });
    };
    Ok(classify_tx_verification(input, &observed))
}

#[must_use]
pub fn build_verification_callback(
    input: &TxVerificationInput,
    result: &TxVerificationResult,
) -> TxVerificationCallback {
    TxVerificationCallback {
        room_id: input.room_id,
        hand_id: input.hand_id,
        seat_id: input.seat_id,
        action_seq: input.action_seq,
        tx_hash: result.tx_hash.clone(),
        status: result.status,
        confirmations: result.confirmations,
        failure_reason: result.failure_reason.clone(),
    }
}

pub async fn dispatch_verification_callback<S: VerificationCallbackSink>(
    sink: &S,
    input: &TxVerificationInput,
    result: &TxVerificationResult,
) -> Result<(), ChainWatcherError> {
    let callback = build_verification_callback(input, result);
    sink.on_verification_result(callback).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationDecision {
    Pending,
    Confirmed,
}

#[must_use]
pub fn check_tx_status_and_identity(
    input: &TxVerificationInput,
    observed: &ObservedTx,
) -> Option<TxVerificationResult> {
    if observed.tx_hash != input.tx_hash {
        return Some(TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Failed,
            confirmations: observed.confirmations,
            failure_reason: Some("tx_hash_mismatch".to_string()),
        });
    }

    if !observed.success {
        return Some(TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Failed,
            confirmations: observed.confirmations,
            failure_reason: Some("tx_failed".to_string()),
        });
    }

    None
}

#[must_use]
pub fn evaluate_confirmations(
    min_confirmations: u64,
    observed_confirmations: u64,
) -> ConfirmationDecision {
    if observed_confirmations < min_confirmations {
        ConfirmationDecision::Pending
    } else {
        ConfirmationDecision::Confirmed
    }
}

#[must_use]
pub fn check_tx_fields(
    input: &TxVerificationInput,
    observed: &ObservedTx,
) -> Option<TxVerificationResult> {
    if observed.to != input.expected_to {
        return Some(TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Unmatched,
            confirmations: observed.confirmations,
            failure_reason: Some("to_mismatch".to_string()),
        });
    }

    if let Some(expected_from) = &input.expected_from
        && observed.from != *expected_from
    {
        return Some(TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Unmatched,
            confirmations: observed.confirmations,
            failure_reason: Some("from_mismatch".to_string()),
        });
    }

    if let Some(expected_amount) = input.expected_amount
        && observed.value != expected_amount
    {
        return Some(TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Unmatched,
            confirmations: observed.confirmations,
            failure_reason: Some("value_mismatch".to_string()),
        });
    }

    None
}

#[must_use]
pub fn classify_tx_verification(
    input: &TxVerificationInput,
    observed: &ObservedTx,
) -> TxVerificationResult {
    if let Some(result) = check_tx_status_and_identity(input, observed) {
        return result;
    }

    if matches!(
        evaluate_confirmations(input.min_confirmations, observed.confirmations),
        ConfirmationDecision::Pending
    ) {
        return TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Pending,
            confirmations: observed.confirmations,
            failure_reason: None,
        };
    }

    if let Some(result) = check_tx_fields(input, observed) {
        return result;
    }

    if input.hand_id.is_none() || input.action_seq.is_none() {
        return TxVerificationResult {
            tx_hash: input.tx_hash.clone(),
            status: VerificationStatus::Late,
            confirmations: observed.confirmations,
            failure_reason: Some("missing_binding_context".to_string()),
        };
    }

    TxVerificationResult {
        tx_hash: input.tx_hash.clone(),
        status: VerificationStatus::Matched,
        confirmations: observed.confirmations,
        failure_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default, Clone)]
    struct RecordingSink {
        events: Arc<Mutex<Vec<TxVerificationCallback>>>,
    }

    #[async_trait]
    impl VerificationCallbackSink for RecordingSink {
        async fn on_verification_result(
            &self,
            callback: TxVerificationCallback,
        ) -> Result<(), ChainWatcherError> {
            self.events.lock().expect("lock").push(callback);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct MockRpcClient {
        calls: Arc<Mutex<u32>>,
        sequence: Arc<Mutex<Vec<Option<ObservedTx>>>>,
    }

    #[async_trait]
    impl EvmLikeTxRpcClient for MockRpcClient {
        async fn get_tx(&self, _tx_hash: &str) -> Result<Option<ObservedTx>, ChainWatcherError> {
            *self.calls.lock().expect("calls lock") += 1;
            let mut seq = self.sequence.lock().expect("seq lock");
            if seq.is_empty() {
                return Ok(None);
            }
            Ok(seq.remove(0))
        }
    }

    fn sample_input() -> TxVerificationInput {
        TxVerificationInput {
            room_id: RoomId::new(),
            hand_id: Some(HandId::new()),
            seat_id: Some(1),
            action_seq: Some(2),
            tx_hash: "0xabc".to_string(),
            expected_to: "room_addr".to_string(),
            expected_from: Some("seat_addr".to_string()),
            expected_amount: Some(Chips(100)),
            min_confirmations: 3,
        }
    }

    fn sample_observed() -> ObservedTx {
        ObservedTx {
            tx_hash: "0xabc".to_string(),
            from: "seat_addr".to_string(),
            to: "room_addr".to_string(),
            value: Chips(100),
            success: true,
            confirmations: 3,
        }
    }

    #[test]
    fn classify_returns_pending_when_confirmations_insufficient() {
        let input = sample_input();
        let mut observed = sample_observed();
        observed.confirmations = 1;
        let result = classify_tx_verification(&input, &observed);
        assert_eq!(result.status, VerificationStatus::Pending);
    }

    #[test]
    fn evaluate_confirmations_respects_threshold() {
        assert_eq!(evaluate_confirmations(3, 2), ConfirmationDecision::Pending);
        assert_eq!(
            evaluate_confirmations(3, 3),
            ConfirmationDecision::Confirmed
        );
    }

    #[test]
    fn check_tx_fields_rejects_to_mismatch() {
        let input = sample_input();
        let mut observed = sample_observed();
        observed.to = "wrong_room".to_string();

        let result = check_tx_fields(&input, &observed).expect("mismatch");
        assert_eq!(result.status, VerificationStatus::Unmatched);
        assert_eq!(result.failure_reason.as_deref(), Some("to_mismatch"));
    }

    #[test]
    fn check_tx_status_and_identity_rejects_failed_tx() {
        let input = sample_input();
        let mut observed = sample_observed();
        observed.success = false;

        let result = check_tx_status_and_identity(&input, &observed).expect("failed");
        assert_eq!(result.status, VerificationStatus::Failed);
        assert_eq!(result.failure_reason.as_deref(), Some("tx_failed"));
    }

    #[test]
    fn classify_returns_unmatched_on_value_mismatch() {
        let input = sample_input();
        let mut observed = sample_observed();
        observed.value = Chips(101);
        let result = classify_tx_verification(&input, &observed);
        assert_eq!(result.status, VerificationStatus::Unmatched);
        assert_eq!(result.failure_reason.as_deref(), Some("value_mismatch"));
    }

    #[test]
    fn classify_returns_failed_on_tx_failure() {
        let input = sample_input();
        let mut observed = sample_observed();
        observed.success = false;
        let result = classify_tx_verification(&input, &observed);
        assert_eq!(result.status, VerificationStatus::Failed);
    }

    #[test]
    fn classify_returns_late_when_binding_context_missing() {
        let mut input = sample_input();
        input.action_seq = None;
        let observed = sample_observed();
        let result = classify_tx_verification(&input, &observed);
        assert_eq!(result.status, VerificationStatus::Late);
    }

    #[test]
    fn classify_returns_matched_when_all_checks_pass() {
        let input = sample_input();
        let observed = sample_observed();
        let result = classify_tx_verification(&input, &observed);
        assert_eq!(result.status, VerificationStatus::Matched);
    }

    #[tokio::test]
    async fn dispatch_callback_forwards_to_sink() {
        let input = sample_input();
        let observed = sample_observed();
        let result = classify_tx_verification(&input, &observed);
        let sink = RecordingSink::default();

        dispatch_verification_callback(&sink, &input, &result)
            .await
            .expect("dispatch");

        let stored = sink.events.lock().expect("lock");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].room_id, input.room_id);
        assert_eq!(stored[0].status, VerificationStatus::Matched);
    }

    #[test]
    fn retry_policy_backoff_is_capped() {
        let retry = RetryPolicy {
            max_attempts: 5,
            base_backoff_ms: 100,
            max_backoff_ms: 250,
        };
        assert_eq!(retry.backoff_ms(0), 100);
        assert_eq!(retry.backoff_ms(1), 200);
        assert_eq!(retry.backoff_ms(2), 250);
    }

    #[tokio::test]
    async fn query_tx_with_retry_retries_until_found() {
        let client = MockRpcClient {
            calls: Arc::new(Mutex::new(0)),
            sequence: Arc::new(Mutex::new(vec![None, None, Some(sample_observed())])),
        };

        let found = query_tx_with_retry(
            &client,
            "0xabc",
            RetryPolicy {
                max_attempts: 4,
                base_backoff_ms: 1,
                max_backoff_ms: 2,
            },
        )
        .await
        .expect("query");

        assert!(found.is_some());
        assert_eq!(*client.calls.lock().expect("calls"), 3);
    }

    #[tokio::test]
    async fn verify_tx_with_retry_returns_pending_when_not_found() {
        let client = MockRpcClient {
            calls: Arc::new(Mutex::new(0)),
            sequence: Arc::new(Mutex::new(vec![None, None])),
        };
        let input = sample_input();
        let result = verify_tx_with_retry(
            &client,
            &input,
            RetryPolicy {
                max_attempts: 2,
                base_backoff_ms: 1,
                max_backoff_ms: 1,
            },
        )
        .await
        .expect("verify");
        assert_eq!(result.status, VerificationStatus::Pending);
        assert_eq!(result.failure_reason.as_deref(), Some("tx_not_found"));
    }

    #[tokio::test]
    async fn evm_chain_watcher_verify_and_dispatch_uses_client_and_sink() {
        let client = MockRpcClient {
            calls: Arc::new(Mutex::new(0)),
            sequence: Arc::new(Mutex::new(vec![Some(sample_observed())])),
        };
        let watcher = EvmChainWatcher::new(
            client.clone(),
            RetryPolicy {
                max_attempts: 1,
                base_backoff_ms: 1,
                max_backoff_ms: 1,
            },
        );
        let sink = RecordingSink::default();
        let input = sample_input();

        let result = watcher
            .verify_and_dispatch(&input, &sink)
            .await
            .expect("verify+dispatch");
        assert_eq!(result.status, VerificationStatus::Matched);
        assert_eq!(*client.calls.lock().expect("calls"), 1);
        assert_eq!(sink.events.lock().expect("events").len(), 1);
    }

    #[test]
    fn parse_hex_quantity_helpers_work() {
        assert_eq!(parse_hex_u64("0x0").expect("u64"), 0);
        assert_eq!(parse_hex_u64("0x10").expect("u64"), 16);
        assert_eq!(parse_hex_u128("0x64").expect("u128"), 100);
    }

    #[tokio::test]
    #[ignore = "requires local anvil binary and free TCP port"]
    async fn reqwest_evm_json_rpc_client_reads_tx_from_anvil() {
        let port = 28545_u16;
        let endpoint = format!("http://127.0.0.1:{port}");
        let mut child = Command::new("anvil")
            .arg("-p")
            .arg(port.to_string())
            .arg("--silent")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn anvil");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let http = reqwest::Client::new();
        let accounts_resp: serde_json::Value = http
            .post(&endpoint)
            .json(&serde_json::json!({
                "jsonrpc":"2.0","id":1,"method":"eth_accounts","params":[]
            }))
            .send()
            .await
            .expect("eth_accounts send")
            .json()
            .await
            .expect("eth_accounts json");
        let accounts = accounts_resp["result"]
            .as_array()
            .expect("accounts array")
            .iter()
            .filter_map(|v| v.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        assert!(accounts.len() >= 2);

        let send_resp: serde_json::Value = http
            .post(&endpoint)
            .json(&serde_json::json!({
                "jsonrpc":"2.0","id":1,"method":"eth_sendTransaction","params":[{
                    "from": accounts[0],
                    "to": accounts[1],
                    "value": "0x1"
                }]
            }))
            .send()
            .await
            .expect("eth_sendTransaction send")
            .json()
            .await
            .expect("eth_sendTransaction json");
        let tx_hash = send_resp["result"].as_str().expect("tx hash").to_string();

        let client = ReqwestEvmJsonRpcClient::new(endpoint);
        let tx = client
            .get_tx(&tx_hash)
            .await
            .expect("get tx")
            .expect("some tx");
        assert_eq!(tx.tx_hash.to_lowercase(), tx_hash.to_lowercase());
        assert_eq!(tx.value, Chips(1));
        assert!(tx.confirmations >= 1);

        let _ = child.kill();
        let _ = child.wait();
    }
}
