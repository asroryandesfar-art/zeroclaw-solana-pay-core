#!/usr/bin/env python3
"""Manual SOL-only devnet payment rehearsal with ephemeral in-memory wallets.

Requires `pip install solana` in an isolated environment. Never writes a keypair,
never accepts a mainnet URL, and prints only public addresses and signatures.
Faucet availability is external; a faucet failure is DEVNET BLOCKED, not PASS.
"""

import base64
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from solders.hash import Hash
from solders.instruction import AccountMeta, Instruction
from solders.keypair import Keypair
from solders.message import MessageV0
from solders.pubkey import Pubkey
from solders.system_program import TransferParams, transfer
from solders.transaction import VersionedTransaction

RPC = "https://api.devnet.solana.com"
BIN = Path(__file__).resolve().parents[1] / "target" / "debug" / "solpay"


def rpc(method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    request = Request(RPC, data=body, headers={"Content-Type": "application/json"})
    try:
        with urlopen(request, timeout=20) as response:
            result = json.load(response)
    except HTTPError as error:
        raise RuntimeError(f"devnet {method}: HTTP {error.code}") from None
    except URLError:
        raise RuntimeError(f"devnet {method}: network unavailable") from None
    if "error" in result:
        raise RuntimeError(f"devnet {method}: {result['error']}")
    return result["result"]


def wait_confirmed(signature, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status = rpc("getSignatureStatuses", [[signature], {"searchTransactionHistory": True}])["value"][0]
        if status and status["err"] is not None:
            raise RuntimeError(f"devnet transaction failed: {status['err']}")
        if status and status["confirmationStatus"] in ("confirmed", "finalized"):
            return
        time.sleep(3)
    raise TimeoutError("devnet confirmation timed out")


def send_sol(payer, recipient, reference, lamports):
    blockhash = rpc("getLatestBlockhash", [{"commitment": "confirmed"}])["value"]["blockhash"]
    system_instruction = transfer(TransferParams(
        from_pubkey=payer.pubkey(), to_pubkey=recipient, lamports=lamports
    ))
    # Solana Pay transfer request: reference is a read-only, non-signer account.
    instruction = Instruction(
        system_instruction.program_id,
        system_instruction.data,
        list(system_instruction.accounts) + [AccountMeta(Pubkey.from_string(reference), False, False)],
    )
    message = MessageV0.try_compile(
        payer.pubkey(), [instruction], [], Hash.from_string(blockhash)
    )
    transaction = VersionedTransaction(message, [payer])
    encoded = base64.b64encode(bytes(transaction)).decode()
    signature = rpc("sendTransaction", [encoded, {"encoding": "base64", "skipPreflight": False}])
    wait_confirmed(signature)
    return signature


def cli(environment, *args):
    process = subprocess.run([str(BIN), *args], env=environment, text=True, capture_output=True)
    if process.returncode != 0:
        raise RuntimeError(f"solpay exited {process.returncode}: {process.stderr.strip()}")
    return json.loads(process.stdout)


def settle_until_evidence(environment, reference, wanted):
    deadline = time.monotonic() + 90
    last = None
    while time.monotonic() < deadline:
        last = cli(environment, "settle", "--reference", reference)
        if last["status"] == wanted:
            return last
        time.sleep(3)
    raise TimeoutError(f"settle did not reach {wanted}; last status={last}")


def main():
    if not BIN.is_file():
        raise RuntimeError("build the debug solpay binary first: cargo build --locked")
    payer = Keypair()
    merchant = Keypair()
    other = Keypair()
    with tempfile.TemporaryDirectory(prefix="solpay-devnet-e2e-") as directory:
        ledger = Path(directory) / "invoices.sqlite"
        environment = {
            "PATH": "/usr/bin:/bin",
            "SOLANA_CLUSTER": "devnet",
            "ALLOW_MAINNET": "false",
            "MERCHANT_WALLET": str(merchant.pubkey()),
            "INVOICE_DB": str(ledger),
            "SOLANA_RPC_PRIMARY": RPC,
            "PAYMENT_COMMITMENT": "confirmed",
            "INVOICE_TTL_SECONDS": "1800",
            "MIN_CHARGE": "0.001",
            "RPC_MAX_RETRIES": "1",
        }
        print("cluster=devnet payer=ephemeral merchant=ephemeral", flush=True)
        # Public devnet faucets are rate-limited and may return transient errors.
        airdrop = None
        for attempt in range(2):
            try:
                airdrop = rpc("requestAirdrop", [str(payer.pubkey()), 50_000_000])
                break
            except RuntimeError:
                if attempt == 0:
                    time.sleep(5)
        if airdrop is None:
            raise RuntimeError("DEVNET BLOCKED: public faucet did not fund the ephemeral payer")
        wait_confirmed(airdrop)
        print("faucet=confirmed", flush=True)

        invoice = cli(environment, "create-url", "--amount", "0.01", "--token", "SOL")
        assert invoice["registered"] is True
        signature = send_sol(payer, merchant.pubkey(), invoice["reference"], 10_000_000)
        paid = settle_until_evidence(environment, invoice["reference"], "paid")
        assert paid["signature"] == signature and paid["newly_settled"] is True
        repeated = cli(environment, "settle", "--reference", invoice["reference"])
        assert repeated["status"] == "paid" and repeated["newly_settled"] is False
        print(f"valid_sol=PASS signature={signature} idempotent=PASS", flush=True)

        unrelated = cli(environment, "create-url", "--amount", "0.01", "--token", "SOL")
        assert cli(environment, "settle", "--reference", unrelated["reference"])["status"] == "pending"
        print("wrong_reference=PASS", flush=True)

        underpaid = cli(environment, "create-url", "--amount", "0.02", "--token", "SOL")
        send_sol(payer, merchant.pubkey(), underpaid["reference"], 10_000_000)
        assert settle_until_evidence(environment, underpaid["reference"], "mismatch")["reason"].startswith("underpaid")
        print("underpayment=PASS", flush=True)

        wrong_wallet = cli(environment, "create-url", "--amount", "0.01", "--token", "SOL")
        send_sol(payer, other.pubkey(), wrong_wallet["reference"], 10_000_000)
        assert settle_until_evidence(environment, wrong_wallet["reference"], "mismatch")["status"] == "mismatch"
        print("wrong_recipient=PASS", flush=True)


if __name__ == "__main__":
    main()
