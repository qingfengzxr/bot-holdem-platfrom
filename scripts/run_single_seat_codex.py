#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import secrets
import subprocess
import sys
from typing import Any, Tuple
from urllib import error as urlerror
from urllib import request


def _gen_x25519_keypair_hex() -> Tuple[str, str]:
    try:
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import x25519

        sk = x25519.X25519PrivateKey.generate()
        pk = sk.public_key()
        secret_hex = sk.private_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PrivateFormat.Raw,
            encryption_algorithm=serialization.NoEncryption(),
        ).hex()
        pub_hex = pk.public_bytes(
            encoding=serialization.Encoding.Raw,
            format=serialization.PublicFormat.Raw,
        ).hex()
        return pub_hex, secret_hex
    except Exception:
        pass

    try:
        from nacl.bindings import crypto_scalarmult_base

        secret = bytearray(secrets.token_bytes(32))
        # X25519 clamping
        secret[0] &= 248
        secret[31] &= 127
        secret[31] |= 64
        public = crypto_scalarmult_base(bytes(secret))
        return public.hex(), bytes(secret).hex()
    except Exception as exc:
        raise RuntimeError(
            "unable to generate x25519 keypair; install 'cryptography' (recommended) or 'pynacl'"
        ) from exc


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="Run single_seat_client with codex wrapper and auto-generated x25519 keys."
    )
    p.add_argument("--rpc-endpoint", default="http://31.97.48.104:9000")
    p.add_argument(
        "--rpc-timeout-sec",
        type=float,
        default=10.0,
        help="timeout seconds for JSON-RPC calls to game server/wallet RPC",
    )
    p.add_argument("--wallet-rpc-url", default="http://31.97.48.104:8545")
    p.add_argument("--room-id", help="UUID room id; optional, auto-select via room.list when omitted")
    p.add_argument(
        "--auto-create-room",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="when --room-id is omitted and room.list is empty, call room.create automatically",
    )
    p.add_argument("--seat-id", default="0")
    p.add_argument(
        "--seat-address",
        help="EVM address for this seat; optional, defaults to first account from wallet RPC",
    )
    p.add_argument(
        "--room-chain-address",
        help="table/room receive address; optional, defaults to seat-address",
    )
    p.add_argument("--chain-id", default="31337")
    p.add_argument("--tx-value", default="1")
    p.add_argument("--tick-ms", default="800")
    p.add_argument("--policy-timeout-ms", default="180000")
    p.add_argument("--codex-wrapper-timeout-ms", default="180000")
    p.add_argument("--policy-context-max-entries", default="16")
    p.add_argument("--codex-wrapper-cmd", default="codex")
    p.add_argument("--codex-wrapper-args", default="")
    p.add_argument(
        "--codex-cli-bin",
        default=os.path.join(
            os.path.dirname(os.path.abspath(__file__)), "codex_decide_wrapper.py"
        ),
    )
    p.add_argument("--cargo-bin", default="single_seat_client")
    p.add_argument("--dry-run", action="store_true")
    return p


def _json_rpc_call(url: str, method: str, params: Any, timeout_sec: float = 10.0) -> Any:
    payload = json.dumps(
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    ).encode()
    req = request.Request(
        url,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with request.urlopen(req, timeout=timeout_sec) as resp:
            body = json.loads(resp.read().decode())
    except TimeoutError as exc:
        raise RuntimeError(
            f"json-rpc {method} timeout after {timeout_sec}s (url={url})"
        ) from exc
    except urlerror.URLError as exc:
        raise RuntimeError(f"json-rpc {method} connect error (url={url}): {exc}") from exc
    if body.get("error"):
        raise RuntimeError(f"json-rpc {method} error: {body['error']}")
    if "result" not in body:
        raise RuntimeError(f"json-rpc {method} missing result")
    return body["result"]


def _resolve_seat_address(args: argparse.Namespace) -> str:
    if args.seat_address:
        return args.seat_address
    try:
        accounts = _json_rpc_call(
            args.wallet_rpc_url, "eth_accounts", [], args.rpc_timeout_sec
        )
    except urlerror.URLError as exc:
        raise RuntimeError(
            "cannot reach WALLET_RPC_URL while resolving seat address: "
            f"{args.wallet_rpc_url}. start anvil/geth first, or pass --seat-address explicitly."
        ) from exc
    except Exception as exc:
        raise RuntimeError(
            "failed to resolve seat address from wallet rpc. "
            "pass --seat-address explicitly."
        ) from exc
    if not isinstance(accounts, list) or not accounts:
        raise RuntimeError("wallet rpc eth_accounts returned no accounts; pass --seat-address")
    first = accounts[0]
    if not isinstance(first, str) or not first.startswith("0x"):
        raise RuntimeError("wallet rpc eth_accounts returned invalid first account")
    return first


def _extract_ok_data(envelope: Any, method: str) -> Any:
    if not isinstance(envelope, dict):
        raise RuntimeError(f"{method} invalid envelope type")
    if not envelope.get("ok", False):
        err = envelope.get("error")
        raise RuntimeError(f"{method} failed: {err}")
    return envelope.get("data")


def _select_room_id_from_list_result(data: Any) -> str | None:
    if not isinstance(data, list) or not data:
        return None
    active = next(
        (
            room
            for room in data
            if isinstance(room, dict)
            and str(room.get("status", "")).lower() == "active"
            and isinstance(room.get("room_id"), str)
        ),
        None,
    )
    if active is not None:
        return active["room_id"]
    first = data[0]
    if isinstance(first, dict) and isinstance(first.get("room_id"), str):
        return first["room_id"]
    return None


def _resolve_room_id(args: argparse.Namespace) -> str | None:
    if args.room_id:
        return args.room_id
    if args.dry_run:
        return None
    list_result = _json_rpc_call(
        args.rpc_endpoint,
        "room.list",
        {"include_inactive": True},
        args.rpc_timeout_sec,
    )
    room_list = _extract_ok_data(list_result, "room.list")
    picked = _select_room_id_from_list_result(room_list)
    if picked:
        return picked
    if not args.auto_create_room:
        return None
    create_result = _json_rpc_call(
        args.rpc_endpoint, "room.create", {}, args.rpc_timeout_sec
    )
    created = _extract_ok_data(create_result, "room.create")
    if not isinstance(created, dict) or not isinstance(created.get("room_id"), str):
        raise RuntimeError("room.create succeeded but returned invalid room summary")
    return created["room_id"]


def main() -> int:
    args = _build_parser().parse_args()
    pub_hex, secret_hex = _gen_x25519_keypair_hex()
    try:
        if args.dry_run and not args.seat_address:
            seat_address = "0x0000000000000000000000000000000000000000"
        else:
            seat_address = _resolve_seat_address(args)
        room_chain_address = args.room_chain_address or seat_address
        room_id = _resolve_room_id(args)
    except Exception as exc:
        print(
            "error: "
            f"{exc}. rpc_endpoint={args.rpc_endpoint} wallet_rpc_url={args.wallet_rpc_url}",
            file=sys.stderr,
        )
        return 2

    env = os.environ.copy()
    env.update(
        {
            "RPC_ENDPOINT": args.rpc_endpoint,
            "WALLET_RPC_URL": args.wallet_rpc_url,
            "SEAT_ID": str(args.seat_id),
            "SEAT_ADDRESS": seat_address,
            "ROOM_CHAIN_ADDRESS": room_chain_address,
            "CHAIN_ID": str(args.chain_id),
            "TX_VALUE": str(args.tx_value),
            "TICK_MS": str(args.tick_ms),
            "POLICY_MODE": "codex_cli",
            "CODEX_CLI_BIN": args.codex_cli_bin,
            "CODEX_WRAPPER_CMD": args.codex_wrapper_cmd,
            "CODEX_WRAPPER_ARGS": args.codex_wrapper_args,
            "CODEX_WRAPPER_TIMEOUT_MS": str(args.codex_wrapper_timeout_ms),
            "POLICY_TIMEOUT_MS": str(args.policy_timeout_ms),
            "POLICY_CONTEXT_MAX_ENTRIES": str(args.policy_context_max_entries),
            "CARD_ENCRYPT_PUBKEY_HEX": pub_hex,
            "CARD_ENCRYPT_SECRET_HEX": secret_hex,
        }
    )
    if room_id:
        env["ROOM_ID"] = room_id
    else:
        env.pop("ROOM_ID", None)

    print("Generated seat crypto keypair:")
    print(f"  CARD_ENCRYPT_PUBKEY_HEX={pub_hex}")
    print(f"  CARD_ENCRYPT_SECRET_HEX={secret_hex}")
    print(f"Resolved EVM seat address: {seat_address}")
    print(f"Resolved room chain address: {room_chain_address}")
    if room_id:
        print(f"Using ROOM_ID: {room_id}")
    else:
        print("ROOM_ID not set; single_seat_client will auto-select via room.list")
    print()
    print("Starting single_seat_client with codex_cli policy...")

    cmd = ["cargo", "run", "-p", "headless-agent-client", "--bin", args.cargo_bin]
    if args.dry_run:
        print("Dry run command:")
        print(" ".join(cmd))
        return 0

    proc = subprocess.Popen(cmd, env=env)
    return proc.wait()


if __name__ == "__main__":
    sys.exit(main())
