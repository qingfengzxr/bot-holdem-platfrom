use agent_auth::evm_address_from_secp256k1_verifying_key;
use async_trait::async_trait;
use k256::ecdsa::SigningKey as EvmSigningKey;
use poker_domain::RoomId;
use reqwest::Client;
use rlp::RlpStream;
use settlement::{
    BatchSettlementTransferReceipt, BatchSettlementTransferRequest, BatchSettlementWalletAdapter,
    SettlementTransferReceipt, SettlementTransferRequest, SettlementTxStatus,
    SettlementWalletAdapter,
};
use sha3::Digest;
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::warn;

use crate::room_service_port::AppRoomService;

#[derive(Clone)]
pub struct RoomScopedRawTxSettlementWalletAdapter {
    endpoint: String,
    client: Client,
    room_service: AppRoomService,
}

impl RoomScopedRawTxSettlementWalletAdapter {
    #[must_use]
    pub fn new(endpoint: impl Into<String>, room_service: AppRoomService) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: Client::new(),
            room_service,
        }
    }

    async fn rpc_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({
            "jsonrpc":"2.0",
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
            .map_err(|e| e.to_string())?;
        let payload: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(err) = payload.get("error") {
            let code = err
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default();
            let msg = err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            return Err(format!("rpc code={code} message={msg}"));
        }
        payload
            .get("result")
            .cloned()
            .ok_or_else(|| "missing result".to_string())
    }

    fn signer_for_room(&self, room_id: RoomId) -> Result<(String, EvmSigningKey), String> {
        let Some((expected_address, private_key_hex)) = self
            .room_service
            .room_chain_signer(room_id)
            .map_err(|e| e.to_string())?
        else {
            return Err(format!("room {} has no chain signer", room_id.0));
        };
        let private_key_bytes =
            hex::decode(private_key_hex).map_err(|e| format!("invalid private key hex: {e}"))?;
        let signing_key = EvmSigningKey::from_slice(&private_key_bytes)
            .map_err(|e| format!("invalid private key bytes: {e}"))?;
        let actual_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        if !actual_address.eq_ignore_ascii_case(&expected_address) {
            return Err(format!(
                "room {} signer address mismatch expected={} actual={}",
                room_id.0, expected_address, actual_address
            ));
        }
        Ok((actual_address, signing_key))
    }

    fn build_legacy_raw_tx(
        chain_id: u64,
        nonce: u64,
        gas_price: u128,
        to_address: &str,
        value: u128,
        data: &[u8],
        signing_key: &EvmSigningKey,
    ) -> Result<String, String> {
        let to = parse_evm_address_20_bytes(to_address)?;
        let gas_limit = if data.is_empty() {
            21_000_u64
        } else {
            250_000_u64
        };

        let mut sighash_stream = RlpStream::new_list(9);
        sighash_stream.append(&nonce);
        sighash_stream.append(&gas_price);
        sighash_stream.append(&gas_limit);
        sighash_stream.append(&to.as_slice());
        sighash_stream.append(&value);
        sighash_stream.append(&data);
        sighash_stream.append(&chain_id);
        sighash_stream.append(&0_u8);
        sighash_stream.append(&0_u8);
        let signing_payload = sighash_stream.out();
        let digest = sha3::Keccak256::digest(signing_payload.as_ref());

        let (sig, recid) = signing_key
            .sign_prehash_recoverable(&digest)
            .map_err(|e| format!("sign tx failed: {e}"))?;
        let sig_bytes = sig.to_bytes();
        let r = ethabi::ethereum_types::U256::from_big_endian(&sig_bytes[..32]);
        let s = ethabi::ethereum_types::U256::from_big_endian(&sig_bytes[32..]);
        let v = u64::from(recid.to_byte()) + 35 + (chain_id * 2);

        let mut raw_stream = RlpStream::new_list(9);
        raw_stream.append(&nonce);
        raw_stream.append(&gas_price);
        raw_stream.append(&gas_limit);
        raw_stream.append(&to.as_slice());
        raw_stream.append(&value);
        raw_stream.append(&data);
        raw_stream.append(&v);
        raw_stream.append(&r);
        raw_stream.append(&s);
        Ok(format!("0x{}", hex::encode(raw_stream.out())))
    }
}

#[async_trait]
impl SettlementWalletAdapter for RoomScopedRawTxSettlementWalletAdapter {
    async fn submit_transfer(&self, request: &SettlementTransferRequest) -> Result<String, String> {
        let (from_address, signing_key) = self.signer_for_room(request.room_id)?;
        let chain_id = parse_hex_u64(
            self.rpc_call("eth_chainId", serde_json::json!([]))
                .await?
                .as_str()
                .ok_or_else(|| "eth_chainId returned non-string".to_string())?,
        )?;
        let nonce = parse_hex_u64(
            self.rpc_call(
                "eth_getTransactionCount",
                serde_json::json!([from_address, "pending"]),
            )
            .await?
            .as_str()
            .ok_or_else(|| "eth_getTransactionCount returned non-string".to_string())?,
        )?;
        let gas_price = parse_hex_u128(
            self.rpc_call("eth_gasPrice", serde_json::json!([]))
                .await?
                .as_str()
                .ok_or_else(|| "eth_gasPrice returned non-string".to_string())?,
        )?;
        let raw_tx = Self::build_legacy_raw_tx(
            chain_id,
            nonce,
            gas_price,
            &request.to_address,
            request.amount.as_u128(),
            &[],
            &signing_key,
        )?;
        let tx_hash = self
            .rpc_call("eth_sendRawTransaction", serde_json::json!([raw_tx]))
            .await?;
        tx_hash
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "eth_sendRawTransaction returned non-string".to_string())
    }

    async fn get_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<SettlementTransferReceipt, String> {
        let receipt = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        if receipt.is_null() {
            return Ok(SettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: SettlementTxStatus::Pending,
                confirmations: 0,
            });
        }
        let latest_block = parse_hex_u64(
            self.rpc_call("eth_blockNumber", serde_json::json!([]))
                .await?
                .as_str()
                .ok_or_else(|| "eth_blockNumber returned non-string".to_string())?,
        )?;
        let block_number = receipt
            .get("blockNumber")
            .and_then(serde_json::Value::as_str)
            .map(parse_hex_u64)
            .transpose()?
            .unwrap_or(0);
        let confirmations = latest_block.saturating_sub(block_number).saturating_add(1);
        let success = receipt
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(parse_hex_u64)
            .transpose()?
            .map(|v| v == 1)
            .unwrap_or(false);
        Ok(SettlementTransferReceipt {
            tx_hash: tx_hash.to_string(),
            status: if success {
                SettlementTxStatus::Confirmed
            } else {
                warn!(tx_hash, "room settlement tx failed");
                SettlementTxStatus::Failed
            },
            confirmations,
        })
    }
}

#[derive(Clone)]
pub struct RoomScopedRawTxBatchSettlementWalletAdapter {
    endpoint: String,
    contract_address: String,
    client: Client,
    room_service: AppRoomService,
    tx_transfer_count: std::sync::Arc<Mutex<HashMap<String, usize>>>,
}

impl RoomScopedRawTxBatchSettlementWalletAdapter {
    #[must_use]
    pub fn new(
        endpoint: impl Into<String>,
        contract_address: impl Into<String>,
        room_service: AppRoomService,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            contract_address: contract_address.into(),
            client: Client::new(),
            room_service,
            tx_transfer_count: std::sync::Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn rpc_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({
            "jsonrpc":"2.0",
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
            .map_err(|e| e.to_string())?;
        let payload: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if let Some(err) = payload.get("error") {
            let code = err
                .get("code")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default();
            let msg = err
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            return Err(format!("rpc code={code} message={msg}"));
        }
        payload
            .get("result")
            .cloned()
            .ok_or_else(|| "missing result".to_string())
    }

    fn signer_for_room(&self, room_id: RoomId) -> Result<(String, EvmSigningKey), String> {
        let Some((expected_address, private_key_hex)) = self
            .room_service
            .room_chain_signer(room_id)
            .map_err(|e| e.to_string())?
        else {
            return Err(format!("room {} has no chain signer", room_id.0));
        };
        let private_key_bytes =
            hex::decode(private_key_hex).map_err(|e| format!("invalid private key hex: {e}"))?;
        let signing_key = EvmSigningKey::from_slice(&private_key_bytes)
            .map_err(|e| format!("invalid private key bytes: {e}"))?;
        let actual_address = evm_address_from_secp256k1_verifying_key(signing_key.verifying_key());
        if !actual_address.eq_ignore_ascii_case(&expected_address) {
            return Err(format!(
                "room {} signer address mismatch expected={} actual={}",
                room_id.0, expected_address, actual_address
            ));
        }
        Ok((actual_address, signing_key))
    }

    fn encode_batch_settlement_call(
        request: &BatchSettlementTransferRequest,
    ) -> Result<Vec<u8>, String> {
        let recipients: Vec<ethabi::Token> = request
            .transfers
            .iter()
            .map(|t| {
                t.to_address
                    .parse()
                    .map(ethabi::Token::Address)
                    .map_err(|e| format!("invalid recipient address {}: {e}", t.to_address))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let amounts: Vec<ethabi::Token> = request
            .transfers
            .iter()
            .map(|t| ethabi::Token::Uint(ethabi::ethereum_types::U256::from(t.amount.as_u128())))
            .collect();
        let settlement_id = build_settlement_batch_id(request.room_id, request.hand_id);

        #[allow(deprecated)]
        let func = ethabi::Function {
            name: "settleBatch".to_string(),
            inputs: vec![
                ethabi::Param {
                    name: "recipients".to_string(),
                    kind: ethabi::ParamType::Array(Box::new(ethabi::ParamType::Address)),
                    internal_type: None,
                },
                ethabi::Param {
                    name: "amounts".to_string(),
                    kind: ethabi::ParamType::Array(Box::new(ethabi::ParamType::Uint(256))),
                    internal_type: None,
                },
                ethabi::Param {
                    name: "settlementId".to_string(),
                    kind: ethabi::ParamType::FixedBytes(32),
                    internal_type: None,
                },
            ],
            outputs: vec![],
            constant: None,
            state_mutability: ethabi::StateMutability::Payable,
        };
        func.encode_input(&[
            ethabi::Token::Array(recipients),
            ethabi::Token::Array(amounts),
            ethabi::Token::FixedBytes(settlement_id.to_vec()),
        ])
        .map_err(|e| format!("abi encode failed: {e}"))
    }
}

#[async_trait]
impl BatchSettlementWalletAdapter for RoomScopedRawTxBatchSettlementWalletAdapter {
    async fn submit_batch_transfer(
        &self,
        request: &BatchSettlementTransferRequest,
    ) -> Result<String, String> {
        if request.transfers.is_empty() {
            return Err("batch transfer list cannot be empty".to_string());
        }
        let total_value = request
            .transfers
            .iter()
            .try_fold(poker_domain::Chips::ZERO, |acc, t| {
                acc.checked_add(t.amount)
            })
            .map_err(|e| format!("batch payout total overflow: {e}"))?;
        let (from_address, signing_key) = self.signer_for_room(request.room_id)?;
        let chain_id = parse_hex_u64(
            self.rpc_call("eth_chainId", serde_json::json!([]))
                .await?
                .as_str()
                .ok_or_else(|| "eth_chainId returned non-string".to_string())?,
        )?;
        let nonce = parse_hex_u64(
            self.rpc_call(
                "eth_getTransactionCount",
                serde_json::json!([from_address, "pending"]),
            )
            .await?
            .as_str()
            .ok_or_else(|| "eth_getTransactionCount returned non-string".to_string())?,
        )?;
        let gas_price = parse_hex_u128(
            self.rpc_call("eth_gasPrice", serde_json::json!([]))
                .await?
                .as_str()
                .ok_or_else(|| "eth_gasPrice returned non-string".to_string())?,
        )?;
        let data = Self::encode_batch_settlement_call(request)?;
        let raw_tx = RoomScopedRawTxSettlementWalletAdapter::build_legacy_raw_tx(
            chain_id,
            nonce,
            gas_price,
            &self.contract_address,
            total_value.as_u128(),
            &data,
            &signing_key,
        )?;
        let tx_hash = self
            .rpc_call("eth_sendRawTransaction", serde_json::json!([raw_tx]))
            .await?;
        let tx_hash_str = tx_hash
            .as_str()
            .ok_or_else(|| "eth_sendRawTransaction returned non-string".to_string())?
            .to_string();
        self.tx_transfer_count
            .lock()
            .map_err(|_| "tx_transfer_count lock poisoned".to_string())?
            .insert(tx_hash_str.clone(), request.transfers.len());
        Ok(tx_hash_str)
    }

    async fn get_batch_transfer_receipt(
        &self,
        tx_hash: &str,
    ) -> Result<BatchSettlementTransferReceipt, String> {
        let receipt = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        if receipt.is_null() {
            return Ok(BatchSettlementTransferReceipt {
                tx_hash: tx_hash.to_string(),
                status: SettlementTxStatus::Pending,
                confirmations: 0,
                applied_count: self
                    .tx_transfer_count
                    .lock()
                    .ok()
                    .and_then(|m| m.get(tx_hash).copied())
                    .unwrap_or(0),
            });
        }
        let latest_block = parse_hex_u64(
            self.rpc_call("eth_blockNumber", serde_json::json!([]))
                .await?
                .as_str()
                .ok_or_else(|| "eth_blockNumber returned non-string".to_string())?,
        )?;
        let block_number = receipt
            .get("blockNumber")
            .and_then(serde_json::Value::as_str)
            .map(parse_hex_u64)
            .transpose()?
            .unwrap_or(0);
        let confirmations = latest_block.saturating_sub(block_number).saturating_add(1);
        let success = receipt
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(parse_hex_u64)
            .transpose()?
            .map(|v| v == 1)
            .unwrap_or(false);
        Ok(BatchSettlementTransferReceipt {
            tx_hash: tx_hash.to_string(),
            status: if success {
                SettlementTxStatus::Confirmed
            } else {
                SettlementTxStatus::Failed
            },
            confirmations,
            applied_count: self
                .tx_transfer_count
                .lock()
                .ok()
                .and_then(|m| m.get(tx_hash).copied())
                .unwrap_or(0),
        })
    }
}

fn parse_evm_address_20_bytes(address: &str) -> Result<[u8; 20], String> {
    let normalized = address.trim();
    let raw = normalized.strip_prefix("0x").unwrap_or(normalized);
    if raw.len() != 40 {
        return Err(format!("invalid address length: {address}"));
    }
    let bytes = hex::decode(raw).map_err(|e| format!("invalid address hex: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| "invalid address byte length".to_string())
}

fn parse_hex_u64(input: &str) -> Result<u64, String> {
    let s = input.trim();
    let raw = s.strip_prefix("0x").unwrap_or(s);
    if raw.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(raw, 16).map_err(|e| e.to_string())
}

fn parse_hex_u128(input: &str) -> Result<u128, String> {
    let s = input.trim();
    let raw = s.strip_prefix("0x").unwrap_or(s);
    if raw.is_empty() {
        return Ok(0);
    }
    u128::from_str_radix(raw, 16).map_err(|e| e.to_string())
}

fn build_settlement_batch_id(room_id: RoomId, hand_id: poker_domain::HandId) -> [u8; 32] {
    let mut hasher = sha3::Keccak256::new();
    hasher.update(room_id.0.as_bytes());
    hasher.update(hand_id.0.as_bytes());
    let digest = hasher.finalize();
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}
