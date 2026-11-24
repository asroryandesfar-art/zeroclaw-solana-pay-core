//! Payment verification — the deterministic verdict at the heart of the system.
//!
//! # Why this exists
//! Everything else exists to get us here safely: deciding, from on-chain
//! evidence alone, whether an invoice has been paid. This is the one place a
//! wrong answer costs real money, so it is a pure function — no network, no
//! clock, no floats — and is exhaustively tested against constructed evidence.
//!
//! # The five checks (all must hold to declare `Paid`)
//! 1. **reference present** — both SOL and SPL transactions must include this
//!    invoice's unique reference key.
//! 2. **exact mint** — funds are the expected USDC mint, not a look-alike token.
//! 3. **correct recipient** — funds landed in the merchant's associated token
//!    account (derived from the merchant wallet + the real mint).
//! 4. **exact amount** — at least the expected base-unit amount arrived.
//! 5. **commitment ≥ required** — the transaction is `confirmed` (or better);
//!    `processed` is never enough.
//!
//! Plus: the transaction must have **succeeded** on-chain (`meta.err == null`).
//!
//! # Threat model
//! The message, the LLM, and the RPC node are all untrusted. None of the five
//! criteria are taken from any of them: `reference`, `mint`, `recipient_ata`,
//! `amount`, and `required_commitment` come from locked config/invoice state.
//! Defended here: fake-USDC (check 2), wrong destination (check 3), underpayment
//! (check 4), acting on droppable transactions (check 5), and replay — a unique
//! reference means a signature for another invoice can neither appear (the RPC
//! query is by reference) nor pass check 1. The caller must still persist a
//! unique reference per invoice and claim a signature atomically across orders.
//!
//! # Why this design is better
//! Separating the pure verdict from the network fetch means the money decision
//! is fully testable offline and cannot be perturbed by a flaky or hostile node.
//! Summation uses `u128` so crafted balances cannot overflow, and there are no
//! panics on any input.

use super::ata::associated_token_address;
use super::commitment::CommitmentLevel;
use super::model::{TransactionEvidence, TransferInstruction};
use super::rpc::{HttpTransport, RpcClient, RpcError};
use crate::money::USDC_DECIMALS;
use solana_pubkey::Pubkey;

const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// The asset an invoice is denominated in.
#[derive(Debug, Clone)]
pub enum ExpectedAsset {
    /// Native SOL: funds must be credited (lamports) to the merchant wallet.
    Sol,
    /// SPL token (e.g. USDC): funds must be credited to the merchant's ATA.
    Spl { mint: Pubkey, recipient_ata: Pubkey },
}

/// The locked expectations an on-chain payment must satisfy. Construct via
/// [`Expected::new`] (SPL/USDC — derives the merchant ATA) or
/// [`Expected::new_sol`] (native SOL).
#[derive(Debug, Clone)]
pub struct Expected {
    pub reference: Pubkey,
    /// The merchant wallet (native SOL destination, and ATA owner for SPL).
    pub recipient: Pubkey,
    pub amount_base_units: u64,
    pub required_commitment: CommitmentLevel,
    pub asset: ExpectedAsset,
    /// Set by the CLI from persisted invoice timestamps. If present, chain
    /// block time must fall within the inclusive invoice validity window.
    pub payment_window: Option<PaymentWindow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaymentWindow {
    pub issued_at_unix: i64,
    pub expires_at_unix: i64,
}

impl PaymentWindow {
    pub fn new(issued_at_unix: i64, expires_at_unix: i64) -> Option<Self> {
        (issued_at_unix > 0 && expires_at_unix > issued_at_unix).then_some(Self {
            issued_at_unix,
            expires_at_unix,
        })
    }
}

impl Expected {
    /// SPL-token invoice (e.g. USDC). Derives the merchant's associated token
    /// account so the caller cannot pass the wrong destination.
    pub fn new(
        reference: Pubkey,
        mint: Pubkey,
        merchant_wallet: Pubkey,
        amount_base_units: u64,
        required_commitment: CommitmentLevel,
    ) -> Self {
        let recipient_ata = associated_token_address(&merchant_wallet, &mint);
        Self {
            reference,
            recipient: merchant_wallet,
            amount_base_units,
            required_commitment,
            asset: ExpectedAsset::Spl {
                mint,
                recipient_ata,
            },
            payment_window: None,
        }
    }

    /// Native SOL invoice: funds are credited directly to the merchant wallet.
    pub fn new_sol(
        reference: Pubkey,
        merchant_wallet: Pubkey,
        amount_base_units: u64,
        required_commitment: CommitmentLevel,
    ) -> Self {
        Self {
            reference,
            recipient: merchant_wallet,
            amount_base_units,
            required_commitment,
            asset: ExpectedAsset::Sol,
            payment_window: None,
        }
    }

    pub fn with_payment_window(mut self, window: PaymentWindow) -> Self {
        self.payment_window = Some(window);
        self
    }
}

/// A signature and its fetched transaction evidence, paired for evaluation.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub signature: String,
    pub commitment: Option<CommitmentLevel>,
    pub evidence: TransactionEvidence,
}

/// The outcome of evaluating a single candidate transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxOutcome {
    Paid,
    InvalidExpectedAmount,
    NotConfirmed,
    TxFailed,
    MissingReference,
    MissingBlockTime,
    OutsidePaymentWindow,
    WrongMint,
    WrongRecipient,
    InvalidTokenAccount,
    MissingTransferProof,
    Underpaid { got: u64 },
}

/// The final verdict for an invoice across all candidate transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Paid { signature: String, slot: u64 },
    Mismatch { reason: String },
    Pending,
}

/// Evaluate one candidate transaction against the locked expectations. Pure.
pub fn evaluate(candidate: &Candidate, expected: &Expected) -> TxOutcome {
    // Keep the public verification library fail-closed even if a caller bypasses
    // the CLI's positive amount validation.
    if expected.amount_base_units == 0 {
        return TxOutcome::InvalidExpectedAmount;
    }
    // Check 5 (commitment) first: never reason about content of a transaction
    // that is not yet confirmed enough to act on. Unknown status is conservative.
    if !candidate
        .commitment
        .is_some_and(|c| c.meets(expected.required_commitment))
    {
        return TxOutcome::NotConfirmed;
    }
    // A failed transaction moved no funds.
    if !candidate.evidence.succeeded {
        return TxOutcome::TxFailed;
    }
    if let Some(window) = expected.payment_window {
        match candidate.evidence.block_time_unix {
            None => return TxOutcome::MissingBlockTime,
            Some(t) if t < window.issued_at_unix || t > window.expires_at_unix => {
                return TxOutcome::OutsidePaymentWindow;
            }
            Some(_) => {}
        }
    }
    // A merchant-wallet transfer alone cannot identify an invoice. Requiring
    // the reference for SOL as well prevents one transfer from settling every
    // same-amount invoice for this wallet. Wallets that omit it remain pending.
    if !candidate
        .evidence
        .contains_account(&expected.reference.to_string())
    {
        return TxOutcome::MissingReference;
    }
    // Checks 2–4 depend on the asset.
    match &expected.asset {
        ExpectedAsset::Spl {
            mint,
            recipient_ata,
        } => evaluate_spl(candidate, expected, mint, recipient_ata),
        ExpectedAsset::Sol => evaluate_sol(candidate, expected),
    }
}

/// SPL-token check: exact mint into the merchant ATA, amount ≥ expected.
fn evaluate_spl(
    candidate: &Candidate,
    expected: &Expected,
    mint: &Pubkey,
    recipient_ata: &Pubkey,
) -> TxOutcome {
    let mint_str = mint.to_string();
    let ata_str = recipient_ata.to_string();
    let merchant_str = expected.recipient.to_string();
    let reference = expected.reference.to_string();

    // u128 sum so crafted values can't wrap.
    let mut received: u128 = 0;
    for d in &candidate.evidence.token_deltas {
        if d.account == ata_str && d.mint == mint_str {
            if d.owner.as_deref() != Some(merchant_str.as_str())
                || d.program_id.as_deref() != Some(TOKEN_PROGRAM_ID)
                || d.decimals != Some(USDC_DECIMALS)
            {
                return TxOutcome::InvalidTokenAccount;
            }
            received += d.increase() as u128;
        }
    }
    if received > 0 {
        let proven: u128 = candidate
            .evidence
            .transfers
            .iter()
            .filter_map(|instruction| match instruction {
                TransferInstruction::Token {
                    destination,
                    mint: instruction_mint,
                    amount,
                    decimals,
                    extra_accounts,
                } if destination == &ata_str
                    && instruction_mint
                        .as_deref()
                        .is_none_or(|value| value == mint_str.as_str())
                    && decimals.is_none_or(|value| value == USDC_DECIMALS)
                    && extra_accounts.iter().any(|account| account == &reference) =>
                {
                    Some(*amount as u128)
                }
                _ => None,
            })
            .sum();
        if proven == 0 {
            return TxOutcome::MissingTransferProof;
        }
        let credited = received.min(proven);
        if credited >= expected.amount_base_units as u128 {
            return TxOutcome::Paid;
        }
        return TxOutcome::Underpaid {
            got: credited as u64,
        };
    }

    // Nothing valid reached us. Diagnose why, deterministically.
    let right_mint_elsewhere = candidate
        .evidence
        .token_deltas
        .iter()
        .any(|d| d.increase() > 0 && d.mint == mint_str && d.account != ata_str);
    let wrong_mint_inflow = candidate
        .evidence
        .token_deltas
        .iter()
        .any(|d| d.increase() > 0 && d.mint != mint_str);

    if right_mint_elsewhere {
        TxOutcome::WrongRecipient
    } else if wrong_mint_inflow {
        TxOutcome::WrongMint
    } else {
        TxOutcome::WrongRecipient
    }
}

/// Native SOL check: lamports credited to the merchant wallet, amount ≥ expected.
fn evaluate_sol(candidate: &Candidate, expected: &Expected) -> TxOutcome {
    let recipient = expected.recipient.to_string();
    let reference = expected.reference.to_string();
    let received = candidate.evidence.lamport_increase(&recipient) as u128;
    if received > 0 {
        let proven: u128 = candidate
            .evidence
            .transfers
            .iter()
            .filter_map(|instruction| match instruction {
                TransferInstruction::Sol {
                    destination,
                    amount,
                    extra_accounts,
                } if destination == &recipient
                    && extra_accounts.iter().any(|account| account == &reference) =>
                {
                    Some(*amount as u128)
                }
                _ => None,
            })
            .sum();
        if proven == 0 {
            return TxOutcome::MissingTransferProof;
        }
        let credited = received.min(proven);
        if credited >= expected.amount_base_units as u128 {
            TxOutcome::Paid
        } else {
            TxOutcome::Underpaid {
                got: credited as u64,
            }
        }
    } else {
        // No SOL reached the merchant wallet (e.g. paid a token, or wrong wallet).
        TxOutcome::WrongRecipient
    }
}

/// Combine per-candidate outcomes into a single verdict.
///
/// Priority: any `Paid` wins (so a duplicate/second valid payment still reads as
/// paid, and the SOP's state guard makes confirmation fire once). Otherwise the
/// most actionable mismatch is reported. Only "not yet" signals (not confirmed,
/// failed, missing reference) leave the invoice `Pending`.
pub fn decide(candidates: &[Candidate], expected: &Expected) -> Verdict {
    if expected.amount_base_units == 0 {
        return Verdict::Mismatch {
            reason: "expected payment amount must be positive".to_string(),
        };
    }
    let mut underpaid: Option<u64> = None;
    let mut wrong_mint = false;
    let mut wrong_recipient = false;
    let mut outside_window = false;
    let mut invalid_token_account = false;
    let mut missing_transfer_proof = false;

    for c in candidates {
        match evaluate(c, expected) {
            TxOutcome::Paid => {
                return Verdict::Paid {
                    signature: c.signature.clone(),
                    slot: c.evidence.slot,
                };
            }
            TxOutcome::Underpaid { got } => underpaid = Some(underpaid.map_or(got, |g| g.max(got))),
            TxOutcome::WrongMint => wrong_mint = true,
            TxOutcome::WrongRecipient => wrong_recipient = true,
            TxOutcome::InvalidTokenAccount => invalid_token_account = true,
            TxOutcome::MissingTransferProof => missing_transfer_proof = true,
            TxOutcome::OutsidePaymentWindow => outside_window = true,
            TxOutcome::NotConfirmed
            | TxOutcome::InvalidExpectedAmount
            | TxOutcome::TxFailed
            | TxOutcome::MissingReference
            | TxOutcome::MissingBlockTime => {}
        }
    }

    if let Some(got) = underpaid {
        return Verdict::Mismatch {
            reason: format!(
                "underpaid: received {got} of {} base units",
                expected.amount_base_units
            ),
        };
    }
    if outside_window {
        return Verdict::Mismatch {
            reason: "payment outside the invoice validity window".to_string(),
        };
    }
    if wrong_mint {
        return Verdict::Mismatch {
            reason: "wrong token mint (not the expected mint)".to_string(),
        };
    }
    if wrong_recipient {
        return Verdict::Mismatch {
            reason: "payment did not reach the merchant token account".to_string(),
        };
    }
    if invalid_token_account {
        return Verdict::Mismatch {
            reason: "token account owner, program, or decimals is invalid".to_string(),
        };
    }
    if missing_transfer_proof {
        return Verdict::Mismatch {
            reason: "no matching reference-bearing transfer instruction".to_string(),
        };
    }
    Verdict::Pending
}

/// Orchestrate verification against the chain: find candidate signatures, fetch
/// each confirmed transaction, and decide. An RPC failure returns `Err` (a
/// transient condition — the caller keeps the invoice PENDING).
///
/// Both assets are queried by their unique invoice reference. SOL wallets that
/// omit the reference cannot be safely attributed to an invoice and will not
/// settle automatically.
pub fn verify_payment<T: HttpTransport>(
    client: &RpcClient<T>,
    expected: &Expected,
    signature_limit: u32,
) -> Result<Verdict, RpcError> {
    verify_payment_with_page_size(client, expected, signature_limit, 1000)
}

fn verify_payment_with_page_size<T: HttpTransport>(
    client: &RpcClient<T>,
    expected: &Expected,
    signature_limit: u32,
    page_size: u32,
) -> Result<Verdict, RpcError> {
    if signature_limit == 0 || !(1..=1000).contains(&page_size) {
        return Err(RpcError::Malformed("invalid signature scan limit".into()));
    }
    let mut candidates = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    let mut scanned = 0u32;
    while scanned < signature_limit {
        let requested = page_size.min(signature_limit - scanned);
        let signatures = client.get_signatures_for_address_page(
            &expected.reference,
            expected.required_commitment,
            requested,
            cursor.as_deref(),
        )?;
        if signatures.is_empty() {
            break;
        }
        if signatures.len() > requested as usize {
            return Err(RpcError::Malformed(
                "RPC returned an oversized signature page".into(),
            ));
        }
        let page_len = signatures.len() as u32;
        cursor = signatures.last().map(|s| s.signature.clone());
        for sig in signatures {
            if !seen.insert(sig.signature.clone()) {
                return Err(RpcError::Malformed(
                    "duplicate signature across RPC pages".into(),
                ));
            }
            // A failed signature moved no funds; skip the extra fetch.
            if sig.failed {
                continue;
            }
            let evidence =
                match client.get_transaction(&sig.signature, expected.required_commitment)? {
                    Some(ev) => ev,
                    None => continue, // dropped between listing and fetch; retry next tick
                };
            let candidate = Candidate {
                signature: sig.signature,
                commitment: sig.confirmation_status,
                evidence,
            };
            if evaluate(&candidate, expected) == TxOutcome::Paid {
                return Ok(Verdict::Paid {
                    signature: candidate.signature,
                    slot: candidate.evidence.slot,
                });
            }
            candidates.push(candidate);
        }
        scanned += page_len;
        if page_len < requested {
            break;
        }
    }

    Ok(decide(&candidates, expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::solana::model::TokenBalanceDelta;

    const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
    const OTHER_MINT: &str = "So11111111111111111111111111111111111111112";
    // A plausible on-curve merchant wallet.
    const MERCHANT: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";

    fn pk(s: &str) -> Pubkey {
        Pubkey::from_str_const(s)
    }

    fn expected(amount: u64, commitment: CommitmentLevel) -> Expected {
        Expected::new(pk(USDC), pk(USDC), pk(MERCHANT), amount, commitment)
    }

    /// Build evidence: a transfer of `post-pre` of `mint` into `account`, with
    /// `reference` present (unless overridden), succeeded, at `slot`.
    fn evidence_with(
        account: &str,
        mint: &str,
        pre: u64,
        post: u64,
        reference: &str,
        succeeded: bool,
    ) -> TransactionEvidence {
        TransactionEvidence {
            slot: 42,
            block_time_unix: Some(42),
            succeeded,
            account_keys: vec!["payer".into(), account.to_string(), reference.to_string()],
            token_deltas: vec![TokenBalanceDelta {
                account: account.to_string(),
                mint: mint.to_string(),
                owner: Some(MERCHANT.to_string()),
                program_id: Some(TOKEN_PROGRAM_ID.to_string()),
                decimals: Some(USDC_DECIMALS),
                pre,
                post,
            }],
            transfers: vec![TransferInstruction::Token {
                destination: account.to_string(),
                mint: Some(mint.to_string()),
                amount: post.saturating_sub(pre),
                decimals: Some(USDC_DECIMALS),
                extra_accounts: vec![reference.to_string()],
            }],
            ..Default::default()
        }
    }

    /// Build native-SOL evidence: `post-pre` lamports credited to `wallet`, with
    /// `reference` present, succeeded.
    fn sol_evidence(wallet: &str, pre: u64, post: u64, reference: &str) -> TransactionEvidence {
        TransactionEvidence {
            slot: 42,
            block_time_unix: Some(42),
            succeeded: true,
            account_keys: vec!["payer".into(), wallet.to_string(), reference.to_string()],
            lamport_deltas: vec![crate::solana::model::AccountLamportDelta {
                account: wallet.to_string(),
                pre,
                post,
            }],
            transfers: vec![TransferInstruction::Sol {
                destination: wallet.to_string(),
                amount: post.saturating_sub(pre),
                extra_accounts: vec![reference.to_string()],
            }],
            ..Default::default()
        }
    }

    fn candidate(ev: TransactionEvidence, commitment: CommitmentLevel) -> Candidate {
        Candidate {
            signature: "sig".into(),
            commitment: Some(commitment),
            evidence: ev,
        }
    }

    fn merchant_ata() -> String {
        associated_token_address(&pk(MERCHANT), &pk(USDC)).to_string()
    }

    fn reference_of(exp: &Expected) -> String {
        exp.reference.to_string()
    }

    // ---- valid payment ----
    #[test]
    fn public_verifier_rejects_zero_amount_for_both_assets() {
        let spl = expected(0, CommitmentLevel::Confirmed);
        let spl_candidate = candidate(
            evidence_with(&merchant_ata(), USDC, 0, 0, &reference_of(&spl), true),
            CommitmentLevel::Confirmed,
        );
        assert_eq!(
            evaluate(&spl_candidate, &spl),
            TxOutcome::InvalidExpectedAmount
        );
        assert!(matches!(
            decide(&[spl_candidate], &spl),
            Verdict::Mismatch { .. }
        ));

        let sol = expected_sol(0);
        let sol_candidate = candidate(
            sol_evidence(MERCHANT, 1, 1, &reference_of(&sol)),
            CommitmentLevel::Confirmed,
        );
        assert_eq!(
            evaluate(&sol_candidate, &sol),
            TxOutcome::InvalidExpectedAmount
        );
        assert!(matches!(
            decide(&[sol_candidate], &sol),
            Verdict::Mismatch { .. }
        ));
    }

    #[test]
    fn valid_payment_is_paid() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::Paid
        );
    }

    #[test]
    fn overpayment_is_paid() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            30_000_000,
            &reference_of(&exp),
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::Paid
        );
    }

    #[test]
    fn finalized_payment_is_paid() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        // Finalized exceeds the required Confirmed threshold.
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Finalized), &exp),
            TxOutcome::Paid
        );
    }

    // ---- incorrect amount ----
    #[test]
    fn underpayment_is_flagged_with_amount() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            10_000_000,
            &reference_of(&exp),
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::Underpaid { got: 10_000_000 }
        );
    }

    // ---- incorrect mint (fake USDC) ----
    #[test]
    fn wrong_mint_is_rejected() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        // Paid into some account with a different mint; never reaches our ATA.
        let ev = evidence_with(
            "attackerAcct",
            OTHER_MINT,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::WrongMint
        );
    }

    // ---- incorrect recipient ----
    #[test]
    fn wrong_recipient_is_rejected() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        // Correct mint, but into a different token account.
        let ev = evidence_with(
            "someoneElseAta",
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::WrongRecipient
        );
    }

    // ---- missing reference / replay attempt ----
    #[test]
    fn missing_reference_is_not_paid() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        // A transfer to our ATA of the right amount, but WITHOUT our reference —
        // e.g. an unrelated transaction, or a replay misattributed to us.
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            "someOtherReference",
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::MissingReference
        );
    }

    // ---- unconfirmed ----
    #[test]
    fn processed_only_is_not_confirmed() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Processed), &exp),
            TxOutcome::NotConfirmed
        );
    }

    #[test]
    fn unknown_commitment_is_not_confirmed() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        let c = Candidate {
            signature: "s".into(),
            commitment: None,
            evidence: ev,
        };
        assert_eq!(evaluate(&c, &exp), TxOutcome::NotConfirmed);
    }

    // ---- failed tx ----
    #[test]
    fn failed_transaction_moves_no_funds() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            false,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::TxFailed
        );
    }

    // ---- decide(): aggregation ----
    #[test]
    fn decide_pending_when_no_candidates() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        assert_eq!(decide(&[], &exp), Verdict::Pending);
    }

    #[test]
    fn decide_paid_returns_signature_and_slot() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        let c = Candidate {
            signature: "abc123".into(),
            commitment: Some(CommitmentLevel::Confirmed),
            evidence: ev,
        };
        assert_eq!(
            decide(&[c], &exp),
            Verdict::Paid {
                signature: "abc123".into(),
                slot: 42
            }
        );
    }

    #[test]
    fn decide_duplicate_payments_still_paid_once() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let mk = |sig: &str| Candidate {
            signature: sig.into(),
            commitment: Some(CommitmentLevel::Confirmed),
            evidence: evidence_with(
                &merchant_ata(),
                USDC,
                0,
                25_000_000,
                &reference_of(&exp),
                true,
            ),
        };
        // Two valid payments -> Paid (the SOP guard makes the confirmation fire once).
        match decide(&[mk("first"), mk("second")], &exp) {
            Verdict::Paid { signature, .. } => assert_eq!(signature, "first"),
            other => panic!("expected Paid, got {other:?}"),
        }
    }

    #[test]
    fn decide_multiple_references_one_valid_is_paid() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let bad = Candidate {
            signature: "bad".into(),
            commitment: Some(CommitmentLevel::Confirmed),
            evidence: evidence_with(
                "someoneElseAta",
                USDC,
                0,
                25_000_000,
                &reference_of(&exp),
                true,
            ),
        };
        let good = Candidate {
            signature: "good".into(),
            commitment: Some(CommitmentLevel::Confirmed),
            evidence: evidence_with(
                &merchant_ata(),
                USDC,
                0,
                25_000_000,
                &reference_of(&exp),
                true,
            ),
        };
        match decide(&[bad, good], &exp) {
            Verdict::Paid { signature, .. } => assert_eq!(signature, "good"),
            other => panic!("expected Paid, got {other:?}"),
        }
    }

    #[test]
    fn decide_underpaid_reports_mismatch() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let c = Candidate {
            signature: "s".into(),
            commitment: Some(CommitmentLevel::Confirmed),
            evidence: evidence_with(
                &merchant_ata(),
                USDC,
                0,
                10_000_000,
                &reference_of(&exp),
                true,
            ),
        };
        match decide(&[c], &exp) {
            Verdict::Mismatch { reason } => assert!(reason.contains("underpaid")),
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn decide_unconfirmed_stays_pending() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let c = candidate(
            evidence_with(
                &merchant_ata(),
                USDC,
                0,
                25_000_000,
                &reference_of(&exp),
                true,
            ),
            CommitmentLevel::Processed,
        );
        assert_eq!(decide(&[c], &exp), Verdict::Pending);
    }

    // ---- summation across multiple deltas into our ATA ----
    #[test]
    fn multiple_inflows_into_ata_sum_up() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let ata = merchant_ata();
        let mut ev = evidence_with(&ata, USDC, 0, 15_000_000, &reference_of(&exp), true);
        ev.token_deltas.push(TokenBalanceDelta {
            account: ata.clone(),
            mint: USDC.to_string(),
            owner: Some(MERCHANT.to_string()),
            program_id: Some(TOKEN_PROGRAM_ID.to_string()),
            decimals: Some(USDC_DECIMALS),
            pre: 15_000_000,
            post: 25_000_000,
        });
        ev.transfers.push(TransferInstruction::Token {
            destination: ata,
            mint: Some(USDC.to_string()),
            amount: 10_000_000,
            decimals: Some(USDC_DECIMALS),
            extra_accounts: vec![reference_of(&exp)],
        });
        // 15,000,000 + 10,000,000 = 25,000,000 exactly.
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::Paid
        );
    }

    #[test]
    fn token_balance_without_a_reference_bearing_transfer_cannot_pay() {
        let exp = expected(25_000_000, CommitmentLevel::Finalized);
        let mut ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        ev.transfers.clear();
        assert_eq!(
            evaluate(&candidate(ev.clone(), CommitmentLevel::Finalized), &exp),
            TxOutcome::MissingTransferProof
        );
        ev.transfers.push(TransferInstruction::Token {
            destination: merchant_ata(),
            mint: Some(USDC.into()),
            amount: 25_000_000,
            decimals: Some(USDC_DECIMALS),
            extra_accounts: vec![OTHER_MINT.into()],
        });
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Finalized), &exp),
            TxOutcome::MissingTransferProof
        );
    }

    #[test]
    fn token_account_authority_program_and_decimals_are_checked() {
        let exp = expected(25_000_000, CommitmentLevel::Finalized);
        for field in 0..3 {
            let mut ev = evidence_with(
                &merchant_ata(),
                USDC,
                0,
                25_000_000,
                &reference_of(&exp),
                true,
            );
            let balance = &mut ev.token_deltas[0];
            match field {
                0 => balance.owner = Some(OTHER_MINT.into()),
                1 => {
                    balance.program_id = Some("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb".into())
                }
                _ => balance.decimals = Some(9),
            }
            assert_eq!(
                evaluate(&candidate(ev, CommitmentLevel::Finalized), &exp),
                TxOutcome::InvalidTokenAccount
            );
        }
    }

    #[test]
    fn instruction_amount_limits_token_credit() {
        let exp = expected(25_000_000, CommitmentLevel::Finalized);
        let mut ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        if let TransferInstruction::Token { amount, .. } = &mut ev.transfers[0] {
            *amount = 10_000_000;
        }
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Finalized), &exp),
            TxOutcome::Underpaid { got: 10_000_000 }
        );
    }

    // ---- native SOL ----
    fn expected_sol(amount: u64) -> Expected {
        Expected::new_sol(pk(USDC), pk(MERCHANT), amount, CommitmentLevel::Confirmed)
    }

    #[test]
    fn sol_payment_to_wallet_is_paid() {
        let exp = expected_sol(1_000_000_000); // 1 SOL
        let ev = sol_evidence(MERCHANT, 0, 1_000_000_000, &reference_of(&exp));
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::Paid
        );
    }

    #[test]
    fn sol_balance_without_matching_system_transfer_cannot_pay() {
        let exp = expected_sol(1_000_000_000);
        let mut ev = sol_evidence(MERCHANT, 0, 1_000_000_000, &reference_of(&exp));
        ev.transfers.clear();
        assert_eq!(
            evaluate(&candidate(ev.clone(), CommitmentLevel::Confirmed), &exp),
            TxOutcome::MissingTransferProof
        );
        ev.transfers.push(TransferInstruction::Sol {
            destination: MERCHANT.into(),
            amount: 1_000_000_000,
            extra_accounts: vec![OTHER_MINT.into()],
        });
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::MissingTransferProof
        );
    }

    #[test]
    fn sol_underpayment_is_flagged() {
        let exp = expected_sol(1_000_000_000);
        let ev = sol_evidence(MERCHANT, 0, 400_000_000, &reference_of(&exp));
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::Underpaid { got: 400_000_000 }
        );
    }

    #[test]
    fn sol_without_invoice_reference_stays_pending() {
        let exp = expected_sol(1_000_000_000);
        let ev = sol_evidence(MERCHANT, 0, 1_000_000_000, "unrelatedReference");
        assert_eq!(
            evaluate(&candidate(ev.clone(), CommitmentLevel::Confirmed), &exp),
            TxOutcome::MissingReference
        );
        assert_eq!(
            decide(&[candidate(ev, CommitmentLevel::Confirmed)], &exp),
            Verdict::Pending
        );
    }

    #[test]
    fn late_or_undated_sol_payment_is_never_paid() {
        let exp =
            expected_sol(1_000_000_000).with_payment_window(PaymentWindow::new(10, 30).unwrap());
        let mut ev = sol_evidence(MERCHANT, 0, 1_000_000_000, &reference_of(&exp));
        assert_eq!(
            evaluate(&candidate(ev.clone(), CommitmentLevel::Confirmed), &exp),
            TxOutcome::OutsidePaymentWindow
        );
        ev.block_time_unix = None;
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::MissingBlockTime
        );
    }

    #[test]
    fn timely_referenced_sol_payment_is_paid() {
        let exp =
            expected_sol(1_000_000_000).with_payment_window(PaymentWindow::new(10, 50).unwrap());
        let ev = sol_evidence(MERCHANT, 0, 1_000_000_000, &reference_of(&exp));
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::Paid
        );
    }

    #[test]
    fn same_sol_transaction_cannot_pay_two_referenced_invoices() {
        let first = expected_sol(1_000_000_000);
        let second = Expected::new_sol(
            pk(OTHER_MINT),
            pk(MERCHANT),
            1_000_000_000,
            CommitmentLevel::Confirmed,
        );
        let ev = sol_evidence(MERCHANT, 0, 1_000_000_000, &reference_of(&first));
        assert_eq!(
            evaluate(&candidate(ev.clone(), CommitmentLevel::Confirmed), &first),
            TxOutcome::Paid
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &second),
            TxOutcome::MissingReference
        );
    }

    #[test]
    fn sol_to_wrong_wallet_is_not_paid() {
        let exp = expected_sol(1_000_000_000);
        // Lamports credited to some other wallet, not the merchant.
        let ev = sol_evidence("someOtherWallet", 0, 1_000_000_000, &reference_of(&exp));
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::WrongRecipient
        );
    }

    #[test]
    fn sol_invoice_ignores_token_transfer() {
        // A USDC token transfer must NOT satisfy a SOL invoice (this is exactly
        // the "paid SOL for a USDC invoice" situation, inverted).
        let exp = expected_sol(1_000_000_000);
        let ev = evidence_with(
            &merchant_ata(),
            USDC,
            0,
            25_000_000,
            &reference_of(&exp),
            true,
        );
        assert_eq!(
            evaluate(&candidate(ev, CommitmentLevel::Confirmed), &exp),
            TxOutcome::WrongRecipient
        );
    }

    // ---- verify_payment orchestration (over a scripted transport) ----

    use super::super::rpc::{HttpTransport as _HttpTransport, TransportError};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::time::Duration;

    struct ScriptedTransport {
        responses: RefCell<VecDeque<Result<String, TransportError>>>,
        requests: Rc<RefCell<Vec<serde_json::Value>>>,
    }

    impl ScriptedTransport {
        fn new(responses: Vec<Result<String, TransportError>>) -> Self {
            Self {
                responses: RefCell::new(responses.into_iter().collect()),
                requests: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    impl _HttpTransport for ScriptedTransport {
        fn post_json(&self, _url: &str, body: &str) -> Result<String, TransportError> {
            self.requests
                .borrow_mut()
                .push(serde_json::from_str(body).unwrap());
            self.responses
                .borrow_mut()
                .pop_front()
                .unwrap_or(Err(TransportError::Network("script exhausted".into())))
        }
    }

    fn client(responses: Vec<Result<String, TransportError>>) -> RpcClient<ScriptedTransport> {
        RpcClient::new(
            ScriptedTransport::new(responses),
            vec!["http://mock".into()],
            0,
            Duration::ZERO,
        )
    }

    fn client_with_requests(
        responses: Vec<Result<String, TransportError>>,
        retries: u32,
    ) -> (
        RpcClient<ScriptedTransport>,
        Rc<RefCell<Vec<serde_json::Value>>>,
    ) {
        let transport = ScriptedTransport::new(responses);
        let requests = Rc::clone(&transport.requests);
        (
            RpcClient::new(
                transport,
                vec!["http://mock".into()],
                retries,
                Duration::ZERO,
            ),
            requests,
        )
    }

    fn sig_envelope(sig: &str, status: &str) -> String {
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": [{ "signature": sig, "confirmationStatus": status, "err": null }]
        })
        .to_string()
    }

    fn tx_envelope(sig: &str, ata: &str, mint: &str, amount: &str, reference: &str) -> String {
        let mut transfer_data = vec![3u8];
        transfer_data.extend_from_slice(&amount.parse::<u64>().unwrap().to_le_bytes());
        let transfer_data = bs58::encode(transfer_data).into_string();
        let result = serde_json::json!({
            "slot": 42,
            "blockTime": 42,
            "meta": {
                "err": null,
                "preBalances": [1000000, 2039280, 0, 0],
                "postBalances": [995000, 2039280, 0, 0],
                "preTokenBalances": [
                    { "accountIndex": 1, "mint": mint, "owner": MERCHANT, "programId": TOKEN_PROGRAM_ID, "uiTokenAmount": { "amount": "0", "decimals": 6 } }
                ],
                "postTokenBalances": [
                    { "accountIndex": 1, "mint": mint, "owner": MERCHANT, "programId": TOKEN_PROGRAM_ID, "uiTokenAmount": { "amount": amount, "decimals": 6 } }
                ]
            },
            "transaction": { "signatures": [sig], "message": { "accountKeys": [
                { "pubkey": "payer" }, { "pubkey": ata }, { "pubkey": reference }, { "pubkey": TOKEN_PROGRAM_ID }
            ], "instructions": [{"programIdIndex":3,"accounts":[0,1,0,2],"data":transfer_data}] } }
        });
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result }).to_string()
    }

    #[test]
    fn verify_payment_happy_path_is_paid() {
        let signature = bs58::encode([1u8; 64]).into_string();
        let exp = expected(25_000_000, CommitmentLevel::Confirmed)
            .with_payment_window(PaymentWindow::new(10, 50).unwrap());
        let reference = reference_of(&exp);
        let c = client(vec![
            Ok(sig_envelope(&signature, "confirmed")),
            Ok(tx_envelope(
                &signature,
                &merchant_ata(),
                USDC,
                "25000000",
                &reference,
            )),
        ]);
        let verdict = verify_payment(&c, &exp, 10).unwrap();
        assert_eq!(
            verdict,
            Verdict::Paid {
                signature,
                slot: 42
            }
        );
    }

    #[test]
    fn verify_payment_late_transaction_is_mismatch() {
        let signature = bs58::encode([2u8; 64]).into_string();
        let exp = expected(25_000_000, CommitmentLevel::Confirmed)
            .with_payment_window(PaymentWindow::new(10, 30).unwrap());
        let c = client(vec![
            Ok(sig_envelope(&signature, "confirmed")),
            Ok(tx_envelope(
                &signature,
                &merchant_ata(),
                USDC,
                "25000000",
                &reference_of(&exp),
            )),
        ]);
        assert!(matches!(
            verify_payment(&c, &exp, 10).unwrap(),
            Verdict::Mismatch { .. }
        ));
    }

    #[test]
    fn verify_payment_referenced_sol_from_rpc_is_paid() {
        let signature = bs58::encode([3u8; 64]).into_string();
        let exp =
            expected_sol(1_000_000_000).with_payment_window(PaymentWindow::new(10, 50).unwrap());
        let mut transfer_data = 2u32.to_le_bytes().to_vec();
        transfer_data.extend_from_slice(&1_000_000_000u64.to_le_bytes());
        let tx = serde_json::json!({
            "slot": 42, "blockTime": 42,
            "meta": {
                "err": null,
                "preBalances": [2000000000u64, 0, 0, 0],
                "postBalances": [999995000u64, 1000000000u64, 0, 0],
                "preTokenBalances": [], "postTokenBalances": []
            },
            "transaction": {
                "signatures": [signature.clone()],
                "message": { "accountKeys": ["payer", MERCHANT, reference_of(&exp), "11111111111111111111111111111111"],
                    "instructions":[{"programIdIndex":3,"accounts":[0,1,2],"data":bs58::encode(transfer_data).into_string()}] }
            }
        });
        let c = client(vec![
            Ok(sig_envelope(&signature, "confirmed")),
            Ok(serde_json::json!({"jsonrpc":"2.0","id":1,"result":tx}).to_string()),
        ]);
        assert_eq!(
            verify_payment(&c, &exp, 10).unwrap(),
            Verdict::Paid {
                signature,
                slot: 42
            }
        );
    }

    #[test]
    fn verify_payment_no_signatures_is_pending() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let c = client(vec![Ok(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "result": []
        })
        .to_string())]);
        assert_eq!(verify_payment(&c, &exp, 10).unwrap(), Verdict::Pending);
    }

    #[test]
    fn verify_payment_rpc_failure_propagates() {
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        // Every attempt times out -> Unavailable (caller keeps invoice PENDING).
        let c = client(vec![Err(TransportError::Timeout)]);
        assert_eq!(verify_payment(&c, &exp, 10), Err(RpcError::Unavailable));
    }

    #[test]
    fn verify_payment_underpaid_is_mismatch() {
        let signature = bs58::encode([4u8; 64]).into_string();
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let reference = reference_of(&exp);
        let c = client(vec![
            Ok(sig_envelope(&signature, "confirmed")),
            Ok(tx_envelope(
                &signature,
                &merchant_ata(),
                USDC,
                "10000000",
                &reference,
            )),
        ]);
        match verify_payment(&c, &exp, 10).unwrap() {
            Verdict::Mismatch { reason } => assert!(reason.contains("underpaid")),
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn paginated_scan_finds_older_payment_and_uses_last_signature_as_cursor() {
        let newer = bs58::encode([11u8; 64]).into_string();
        let cursor = bs58::encode([12u8; 64]).into_string();
        let paid = bs58::encode([13u8; 64]).into_string();
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let first_page = serde_json::json!({"jsonrpc":"2.0","id":1,"result":[
            {"signature":newer,"confirmationStatus":"confirmed","err":{"failure":true}},
            {"signature":cursor,"confirmationStatus":"confirmed","err":{"failure":true}}
        ]})
        .to_string();
        let second_page = serde_json::json!({"jsonrpc":"2.0","id":1,"result":[
            {"signature":paid,"confirmationStatus":"confirmed","err":null}
        ]})
        .to_string();
        let (c, requests) = client_with_requests(
            vec![
                Ok(first_page),
                Ok(second_page),
                Ok(tx_envelope(
                    &paid,
                    &merchant_ata(),
                    USDC,
                    "25000000",
                    &reference_of(&exp),
                )),
            ],
            0,
        );
        assert!(matches!(
            verify_payment_with_page_size(&c, &exp, 5, 2),
            Ok(Verdict::Paid { .. })
        ));
        let calls = requests.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0]["params"][1]["limit"], 2);
        assert_eq!(calls[1]["params"][1]["before"], cursor);
        assert_eq!(calls[1]["params"][1]["limit"], 2);
    }

    #[test]
    fn paginated_scan_rejects_duplicate_and_partial_failure() {
        let first = bs58::encode([21u8; 64]).into_string();
        let second = bs58::encode([22u8; 64]).into_string();
        let page = serde_json::json!({"jsonrpc":"2.0","id":1,"result":[
            {"signature":first,"confirmationStatus":"confirmed","err":{"failure":true}},
            {"signature":second,"confirmationStatus":"confirmed","err":{"failure":true}}
        ]})
        .to_string();
        let duplicate = serde_json::json!({"jsonrpc":"2.0","id":1,"result":[
            {"signature":second,"confirmationStatus":"confirmed","err":{"failure":true}}
        ]})
        .to_string();
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        let (c, _) = client_with_requests(vec![Ok(page.clone()), Ok(duplicate)], 0);
        assert!(matches!(
            verify_payment_with_page_size(&c, &exp, 5, 2),
            Err(RpcError::Malformed(_))
        ));

        let (c, _) = client_with_requests(vec![Ok(page.clone()), Err(TransportError::Timeout)], 0);
        assert_eq!(
            verify_payment_with_page_size(&c, &exp, 5, 2),
            Err(RpcError::Unavailable)
        );

        let (c, requests) = client_with_requests(
            vec![
                Ok(page),
                Err(TransportError::HttpStatus(429)),
                Ok(serde_json::json!({"jsonrpc":"2.0","id":1,"result":[]}).to_string()),
            ],
            1,
        );
        assert_eq!(
            verify_payment_with_page_size(&c, &exp, 5, 2).unwrap(),
            Verdict::Pending
        );
        assert_eq!(requests.borrow().len(), 3);
    }

    #[test]
    fn paginated_scan_is_bounded_even_if_every_page_is_full() {
        let first = bs58::encode([31u8; 64]).into_string();
        let second = bs58::encode([32u8; 64]).into_string();
        let page = serde_json::json!({"jsonrpc":"2.0","id":1,"result":[
            {"signature":first,"confirmationStatus":"confirmed","err":{"failure":true}},
            {"signature":second,"confirmationStatus":"confirmed","err":{"failure":true}}
        ]})
        .to_string();
        let (c, requests) = client_with_requests(vec![Ok(page)], 0);
        let exp = expected(25_000_000, CommitmentLevel::Confirmed);
        assert_eq!(
            verify_payment_with_page_size(&c, &exp, 2, 2).unwrap(),
            Verdict::Pending
        );
        assert_eq!(requests.borrow().len(), 1);
    }
}
