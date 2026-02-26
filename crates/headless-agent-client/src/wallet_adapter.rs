use async_trait::async_trait;
use poker_domain::{Chips, RoomId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum WalletAdapterError {
    #[error("wallet not configured")]
    NotConfigured,
    #[error("rpc unavailable")]
    RpcUnavailable,
    #[error("http error: {0}")]
    Http(String),
    #[error("rpc error: {0}")]
    Rpc(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvmTransferRequest {
    pub room_id: RoomId,
    pub from: String,
    pub to: String,
    pub value: Chips,
    pub chain_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvmTransferReceipt {
    pub tx_hash: String,
    pub submitted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvmTxReceiptStatus {
    pub tx_hash: String,
    pub confirmations: Option<u64>,
    pub success: Option<bool>,
}

#[async_trait]
pub trait WalletAdapter: Send + Sync {
    async fn transfer_to_room(
        &self,
        request: EvmTransferRequest,
    ) -> Result<EvmTransferReceipt, WalletAdapterError>;

    async fn sign_personal_message(
        &self,
        from: &str,
        message: &[u8],
    ) -> Result<String, WalletAdapterError>;
}

// eSpace is EVM-compatible, so future production implementation can use alloy/ethers.
// Local/dev can use Foundry Anvil.
#[derive(Debug, Default, Clone)]
pub struct MockEvmWalletAdapter;

#[async_trait]
impl WalletAdapter for MockEvmWalletAdapter {
    async fn transfer_to_room(
        &self,
        _request: EvmTransferRequest,
    ) -> Result<EvmTransferReceipt, WalletAdapterError> {
        Ok(EvmTransferReceipt {
            tx_hash: format!("0x{}", Uuid::now_v7().simple()),
            submitted: true,
        })
    }

    async fn sign_personal_message(
        &self,
        _from: &str,
        _message: &[u8],
    ) -> Result<String, WalletAdapterError> {
        Ok(format!("0x{}", "00".repeat(65)))
    }
}

#[derive(Debug, Clone)]
pub struct JsonRpcEvmWalletAdapter {
    endpoint: String,
    client: reqwest::Client,
}

impl JsonRpcEvmWalletAdapter {
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }

    async fn rpc_call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, WalletAdapterError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| WalletAdapterError::Http(e.to_string()))?;
        let payload: JsonRpcResponse<T> = resp
            .json()
            .await
            .map_err(|e| WalletAdapterError::Http(e.to_string()))?;
        if let Some(err) = payload.error {
            return Err(WalletAdapterError::Rpc(format!(
                "code={} message={}",
                err.code, err.message
            )));
        }
        payload
            .result
            .ok_or_else(|| WalletAdapterError::Rpc("missing result".to_string()))
    }

    pub async fn list_accounts(&self) -> Result<Vec<String>, WalletAdapterError> {
        self.rpc_call("eth_accounts", serde_json::json!([])).await
    }

    pub async fn chain_id(&self) -> Result<u64, WalletAdapterError> {
        let raw: String = self.rpc_call("eth_chainId", serde_json::json!([])).await?;
        parse_hex_u64(&raw)
    }

    pub async fn get_tx_receipt_status(
        &self,
        tx_hash: &str,
    ) -> Result<Option<EvmTxReceiptStatus>, WalletAdapterError> {
        let receipt: Option<EthTransactionReceipt> = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        let Some(receipt) = receipt else {
            return Ok(None);
        };

        let confirmations = receipt
            .confirmations
            .as_deref()
            .map(parse_hex_u64)
            .transpose()?;
        let success = receipt
            .status
            .as_deref()
            .map(parse_hex_u64)
            .transpose()?
            .map(|v| v == 1);
        Ok(Some(EvmTxReceiptStatus {
            tx_hash: tx_hash.to_string(),
            confirmations,
            success,
        }))
    }
}

#[async_trait]
impl WalletAdapter for JsonRpcEvmWalletAdapter {
    async fn transfer_to_room(
        &self,
        request: EvmTransferRequest,
    ) -> Result<EvmTransferReceipt, WalletAdapterError> {
        let value_hex = format!("0x{:x}", request.value.0);
        let tx_hash: String = self
            .rpc_call(
                "eth_sendTransaction",
                serde_json::json!([{
                    "from": request.from,
                    "to": request.to,
                    "value": value_hex,
                }]),
            )
            .await?;
        Ok(EvmTransferReceipt {
            tx_hash,
            submitted: true,
        })
    }

    async fn sign_personal_message(
        &self,
        from: &str,
        message: &[u8],
    ) -> Result<String, WalletAdapterError> {
        let data = format!("0x{}", hex::encode(message));
        self.rpc_call("personal_sign", serde_json::json!([data, from])).await
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct EthTransactionReceipt {
    status: Option<String>,
    confirmations: Option<String>,
}

fn parse_hex_u64(input: &str) -> Result<u64, WalletAdapterError> {
    let s = input.trim();
    let raw = s.strip_prefix("0x").unwrap_or(s);
    if raw.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(raw, 16).map_err(|e| WalletAdapterError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    #[tokio::test]
    async fn mock_wallet_adapter_returns_tx_hash() {
        let adapter = MockEvmWalletAdapter;
        let receipt = adapter
            .transfer_to_room(EvmTransferRequest {
                room_id: RoomId::new(),
                from: "0xfrom".to_string(),
                to: "0xto".to_string(),
                value: Chips(10),
                chain_id: 71,
            })
            .await
            .expect("transfer");
        assert!(receipt.submitted);
        assert!(receipt.tx_hash.starts_with("0x"));
    }

    #[test]
    fn parse_hex_u64_works_for_evm_quantities() {
        assert_eq!(parse_hex_u64("0x0").expect("zero"), 0);
        assert_eq!(parse_hex_u64("0x1").expect("one"), 1);
        assert_eq!(parse_hex_u64("0x10").expect("sixteen"), 16);
    }

    #[tokio::test]
    #[ignore = "requires local anvil binary and free TCP port"]
    async fn json_rpc_wallet_adapter_can_send_tx_against_anvil() {
        let port = 18545_u16;
        let mut child = Command::new("anvil")
            .arg("-p")
            .arg(port.to_string())
            .arg("--silent")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn anvil");

        tokio::time::sleep(Duration::from_millis(500)).await;

        let adapter = JsonRpcEvmWalletAdapter::new(format!("http://127.0.0.1:{port}"));
        let chain_id = adapter.chain_id().await.expect("chain id");
        assert_eq!(chain_id, 31_337);
        let accounts = adapter.list_accounts().await.expect("accounts");
        assert!(accounts.len() >= 2);

        let receipt = adapter
            .transfer_to_room(EvmTransferRequest {
                room_id: RoomId::new(),
                from: accounts[0].clone(),
                to: accounts[1].clone(),
                value: Chips(1),
                chain_id: 31337,
            })
            .await
            .expect("transfer");
        assert!(receipt.tx_hash.starts_with("0x"));
        let polled = adapter
            .get_tx_receipt_status(&receipt.tx_hash)
            .await
            .expect("receipt status");
        assert!(polled.is_some());

        let _ = child.kill();
        let _ = child.wait();
    }
}
