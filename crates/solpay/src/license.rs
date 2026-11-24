//! Native verifier for the Worker/Node `ZSP1` Ed25519 entitlement format.
//! The issuer is the trusted SPKI public key, not a self-asserted payload field.
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const PRODUCT: &str = "zeroclaw-solana-pay";
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LicensePayload {
    pub schema: u8,
    pub product: String,
    pub license_id: String,
    pub buyer_id: String,
    pub purchased_at: String,
    pub usage: String,
    pub commercial_use: bool,
    pub non_exclusive: bool,
    pub price_usd: u16,
    pub updates_through: String,
    pub support_through: String,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Benefit {
    Use,
    Updates,
    Support,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    pub valid: bool,
    pub product: String,
    pub license_id: String,
    pub buyer_id: String,
    pub usage: String,
    pub updates_through: String,
    pub support_through: String,
    pub updates_available: bool,
    pub support_available: bool,
}

pub fn public_key_from_pem(pem: &str) -> Result<[u8; 32], &'static str> {
    let normalized = pem.trim().replace("\r\n", "\n");
    let body = normalized
        .strip_prefix("-----BEGIN PUBLIC KEY-----\n")
        .and_then(|s| s.strip_suffix("\n-----END PUBLIC KEY-----"))
        .ok_or("invalid issuer public key PEM")?;
    if !body.lines().all(|line| {
        !line.is_empty()
            && line
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
    }) {
        return Err("invalid issuer public key PEM");
    }
    let der = STANDARD
        .decode(body.replace('\n', ""))
        .map_err(|_| "invalid issuer public key PEM")?;
    if der.len() != 44 || der[..12] != SPKI_PREFIX {
        return Err("issuer public key must be Ed25519 SPKI");
    }
    let mut raw = [0; 32];
    raw.copy_from_slice(&der[12..]);
    Ok(raw)
}

pub fn issuer_fingerprint(pem: &str) -> Result<String, &'static str> {
    let raw = public_key_from_pem(pem)?;
    let mut der = Vec::from(SPKI_PREFIX);
    der.extend_from_slice(&raw);
    Ok(ring::digest::digest(&ring::digest::SHA256, &der)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(feature = "commercial-package")]
pub fn embedded_issuer() -> &'static str {
    include_str!(env!("ZSP_ISSUER_PUBLIC_KEY_PATH"))
}

pub fn installed_license_path() -> Result<PathBuf, &'static str> {
    if let Some(path) = std::env::var_os("ZSP_LICENSE_FILE") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("ZSP_LICENSE_FILE must be absolute");
        }
        return Ok(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or("license config directory unavailable")?;
    Ok(base.join("zeroclaw-solana-pay").join("license.key"))
}

pub fn read_private_license(path: &Path) -> Result<String, &'static str> {
    let info = std::fs::symlink_metadata(path).map_err(|_| "license not activated")?;
    if !info.is_file() || info.file_type().is_symlink() {
        return Err("license file must be regular");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if info.permissions().mode() & 0o077 != 0 {
            return Err("license file must be owner-private");
        }
    }
    let key = std::fs::read_to_string(path).map_err(|_| "license unreadable")?;
    if key.len() > 4097 {
        return Err("license key too long");
    }
    Ok(key)
}

#[cfg(feature = "commercial-package")]
pub fn check_installed() -> Result<(), &'static str> {
    let key = read_private_license(&installed_license_path()?)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock invalid")?
        .as_millis() as i64;
    verify(&key, embedded_issuer(), now, Benefit::Use).map(|_| ())
}

fn decode_part(part: &str) -> Result<Vec<u8>, &'static str> {
    if part.is_empty()
        || !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid license encoding");
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| "invalid license encoding")?;
    if URL_SAFE_NO_PAD.encode(&bytes) != part {
        return Err("noncanonical license encoding");
    }
    Ok(bytes)
}

pub fn verify(
    key: &str,
    issuer_pem: &str,
    now_ms: i64,
    benefit: Benefit,
) -> Result<LicenseStatus, &'static str> {
    let key = key.trim();
    if key.len() > 4096 {
        return Err("license key too long");
    }
    let mut parts = key.split('.');
    if parts.next() != Some("ZSP1") {
        return Err("invalid license prefix");
    }
    let body = decode_part(parts.next().ok_or("invalid license key")?)?;
    let signature = decode_part(parts.next().ok_or("invalid license key")?)?;
    if parts.next().is_some() || signature.len() != 64 {
        return Err("invalid license signature");
    }
    let raw_key = public_key_from_pem(issuer_pem)?;
    UnparsedPublicKey::new(&ED25519, raw_key)
        .verify(&body, &signature)
        .map_err(|_| "license signature invalid")?;
    let payload: LicensePayload =
        serde_json::from_slice(&body).map_err(|_| "invalid license payload")?;
    validate_contract(&payload)?;
    let updates_ms = parse_utc(&payload.updates_through)
        .ok_or("invalid update expiry")?
        .0;
    let support_ms = parse_utc(&payload.support_through)
        .ok_or("invalid support expiry")?
        .0;
    if matches!(benefit, Benefit::Updates) && now_ms >= updates_ms {
        return Err("updates entitlement expired");
    }
    if matches!(benefit, Benefit::Support) && now_ms >= support_ms {
        return Err("support entitlement expired");
    }
    Ok(LicenseStatus {
        valid: true,
        product: payload.product,
        license_id: payload.license_id,
        buyer_id: payload.buyer_id,
        usage: payload.usage,
        updates_through: payload.updates_through,
        support_through: payload.support_through,
        updates_available: now_ms < updates_ms,
        support_available: now_ms < support_ms,
    })
}

fn validate_contract(p: &LicensePayload) -> Result<(), &'static str> {
    if p.schema != 1
        || p.product != PRODUCT
        || p.usage != "perpetual"
        || !p.commercial_use
        || !p.non_exclusive
        || p.price_usd != 200
    {
        return Err("invalid license entitlement");
    }
    let id = p.license_id.as_bytes();
    if id.len() != 36
        || !id.iter().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                *b == b'-'
            } else if i == 14 {
                *b == b'4'
            } else if i == 19 {
                matches!(*b, b'8' | b'9' | b'a' | b'b')
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(b)
            }
        })
    {
        return Err("invalid license ID");
    }
    if p.buyer_id.is_empty()
        || p.buyer_id.len() > 64
        || !p
            .buyer_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("invalid buyer ID");
    }
    let (bought, y, m, d, hh, mm, ss, ms) =
        parse_utc(&p.purchased_at).ok_or("invalid purchase timestamp")?;
    if p.purchased_at.len() != 24 {
        return Err("invalid purchase timestamp");
    }
    // JavaScript Date.UTC maps years 0..99 to 1900..1999. The Worker uses
    // Date.UTC for the anniversary, so historical dates follow that rule.
    let update_year = if y < 99 { y + 1901 } else { y + 1 };
    let update_day = d.min(days_in_month(update_year, m));
    let expected_updates = stamp(update_year, m, update_day, hh, mm, ss, ms);
    let (sy, sm, sd) = civil_from_days(bought.div_euclid(86_400_000) + 90);
    let expected_support = stamp(sy, sm, sd, hh, mm, ss, ms);
    if p.updates_through != expected_updates || p.support_through != expected_support {
        return Err("invalid license entitlement dates");
    }
    Ok(())
}

fn leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}
fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64 - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = m as i64 + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    (y as i32, m as u32, d as u32)
}
fn stamp(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32, ms: u32) -> String {
    let year = if y > 9999 {
        format!("+{y:06}")
    } else {
        format!("{y:04}")
    };
    format!("{year}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{ms:03}Z")
}
type UtcParts = (i64, i32, u32, u32, u32, u32, u32, u32);
fn parse_utc(s: &str) -> Option<UtcParts> {
    let b = s.as_bytes();
    let extra = if b.first() == Some(&b'+') { 3 } else { 0 };
    if b.len() != 24 + extra
        || ![4, 7, 10, 13, 16, 19, 23]
            .iter()
            .map(|i| i + extra)
            .zip(*b"--T::.Z")
            .all(|(i, c)| b[i] == c)
    {
        return None;
    }
    if !b.iter().enumerate().all(|(i, c)| {
        i == 0 && extra == 3
            || [4, 7, 10, 13, 16, 19, 23].iter().any(|p| *p + extra == i)
            || c.is_ascii_digit()
    }) {
        return None;
    }
    let n = |a, z| s[a..z].parse::<u32>().ok();
    let (y, m, d, hh, mm, ss, ms) = (
        n(if extra == 3 { 1 } else { 0 }, 4 + extra)? as i32,
        n(5 + extra, 7 + extra)?,
        n(8 + extra, 10 + extra)?,
        n(11 + extra, 13 + extra)?,
        n(14 + extra, 16 + extra)?,
        n(17 + extra, 19 + extra)?,
        n(20 + extra, 23 + extra)?,
    );
    if (extra == 3 && y < 10_000)
        || !(1..=12).contains(&m)
        || d < 1
        || d > days_in_month(y, m)
        || hh > 23
        || mm > 59
        || ss > 59
    {
        return None;
    }
    let total = days_from_civil(y, m, d) * 86_400_000
        + (hh as i64 * 3600 + mm as i64 * 60 + ss as i64) * 1000
        + ms as i64;
    Some((total, y, m, d, hh, mm, ss, ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_matches_worker_contract() {
        let p = LicensePayload {
            schema: 1,
            product: PRODUCT.into(),
            license_id: "12345678-1234-4123-8123-123456789abc".into(),
            buyer_id: "buyer_1".into(),
            purchased_at: "2024-02-29T12:34:56.789Z".into(),
            usage: "perpetual".into(),
            commercial_use: true,
            non_exclusive: true,
            price_usd: 200,
            updates_through: "2025-02-28T12:34:56.789Z".into(),
            support_through: "2024-05-29T12:34:56.789Z".into(),
        };
        assert!(validate_contract(&p).is_ok());
        assert_eq!(parse_utc("2024-02-30T00:00:00.000Z"), None);
    }
}
