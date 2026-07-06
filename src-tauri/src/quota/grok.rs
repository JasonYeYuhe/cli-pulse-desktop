//! Grok usage collection — port of macOS `GrokCollector`.
//!
//! Endpoint: `POST grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig`
//! — a **gRPC-web** unary call (`Content-Type: application/grpc-web+proto`) with
//! an empty request frame (the RPC takes no args). Auth: a grok.com session
//! `Cookie:` (env `GROK_COOKIE` or the Settings-stored `grok_cookie`) OR a
//! Bearer token (env `GROK_TOKEN`). Manual only.
//!
//! The response is a **schema-less protobuf** — the Mac reverse-engineered a
//! heuristic scan (there is no `.proto`): used-percent = the `fixed32`/Float
//! whose field path ends in `1` (value 0…100, shallowest then earliest); reset
//! = a future unix-timestamp varint (seconds or millis), preferring path
//! `[1,5,1]`. This whole scan + framing is ported **verbatim** from the proven
//! Mac collector. Primary output is a Credits `%`-gauge (`.quota`); if the scan
//! finds no usable percent it degrades to a 0/0 "connected" snapshot.
//!
//! ⚠️ Because the field-path heuristics were tuned against real Grok responses
//! (which CI can't reach), the byte-level scanner is exhaustively unit-tested
//! against synthetic frames, but a live grok.com cookie is the only true
//! end-to-end check — same caveat as every other collector, amplified by the
//! schema-less parse.

use std::collections::HashMap;
use std::time::Duration;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const ENDPOINT: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
const TIMEOUT: Duration = Duration::from_secs(15);

enum Auth {
    Cookie(String),
    Bearer(String),
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let auth = match resolve_auth() {
        Some(a) => a,
        None => {
            log::debug!("[Grok] no cookie/token (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let (grpc_headers, body) = fetch_once(&auth).await?;
    validate_grpc_status(&grpc_headers)?;
    validate_grpc_status(&grpc_web_trailer_fields(&body))?;
    let now_ts = chrono::Utc::now().timestamp();
    let snap = parse_grpc_web_response(&body, now_ts)?;
    Ok(Some(build_snapshot(&snap)))
}

fn resolve_auth() -> Option<Auth> {
    if let Ok(c) = std::env::var("GROK_COOKIE") {
        let c = c.trim();
        if !c.is_empty() {
            return Some(Auth::Cookie(c.to_string()));
        }
    }
    if let Ok(t) = std::env::var("GROK_TOKEN") {
        let t = t.trim();
        if !t.is_empty() {
            return Some(Auth::Bearer(t.to_string()));
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.grok_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(Auth::Cookie)
}

async fn fetch_once(auth: &Auth) -> Result<(HashMap<String, String>, Vec<u8>), CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    // gRPC-web frame: 1-byte flag (0 = data) + 4-byte big-endian length (0).
    let mut req = client
        .post(ENDPOINT)
        .body(vec![0x00u8, 0x00, 0x00, 0x00, 0x00])
        .header("Origin", "https://grok.com")
        .header("Referer", "https://grok.com/?_s=usage")
        .header("Accept", "*/*")
        .header("Content-Type", "application/grpc-web+proto")
        .header("x-grpc-web", "1")
        .header("x-user-agent", "connect-es/2.1.1")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36",
        );
    req = match auth {
        Auth::Cookie(h) => req.header("Cookie", h),
        Auth::Bearer(t) => req.header("Authorization", format!("Bearer {t}")),
    };
    let resp = req
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("request: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(CollectorError::Http(format!("HTTP {}", status.as_u16())));
    }
    // grpc-status can ride on the HTTP headers AND/OR the trailer frame.
    let grpc_headers: HashMap<String, String> = resp
        .headers()
        .iter()
        .filter(|(k, _)| k.as_str().to_ascii_lowercase().starts_with("grpc-"))
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_ascii_lowercase(), s.trim().to_string()))
        })
        .collect();
    let body = resp
        .bytes()
        .await
        .map_err(|e| CollectorError::Http(format!("body: {e}")))?
        .to_vec();
    Ok((grpc_headers, body))
}

fn validate_grpc_status(fields: &HashMap<String, String>) -> Result<(), CollectorError> {
    let Some(raw) = fields.get("grpc-status") else {
        return Ok(());
    };
    let Ok(status) = raw.parse::<i64>() else {
        return Ok(());
    };
    if status == 0 {
        return Ok(());
    }
    if status == 16 {
        return Err(CollectorError::Http("Grok: unauthenticated".to_string()));
    }
    let msg = fields.get("grpc-message").cloned().unwrap_or_default();
    Err(CollectorError::SchemaOrIo(format!(
        "Grok: RPC status {status} {msg}"
    )))
}

// ── gRPC-web framing (5-byte prefix: 1 flag byte + 4 big-endian length) ──

fn frame_len(data: &[u8], i: usize) -> usize {
    ((data[i + 1] as usize) << 24)
        | ((data[i + 2] as usize) << 16)
        | ((data[i + 3] as usize) << 8)
        | (data[i + 4] as usize)
}

/// Payloads of data frames (flag bit 0x80 clear).
fn grpc_web_data_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let length = frame_len(data, index);
        let start = index + 5;
        let end = start + length;
        if end > data.len() {
            break;
        }
        if flags & 0x80 == 0 {
            frames.push(data[start..end].to_vec());
        }
        index = end;
    }
    frames
}

/// Trailer frames (flag bit 0x80 set) carry `grpc-status`/`grpc-message` text.
fn grpc_web_trailer_fields(data: &[u8]) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let length = frame_len(data, index);
        let start = index + 5;
        let end = start + length;
        if end > data.len() {
            break;
        }
        if flags & 0x80 != 0 {
            if let Ok(text) = std::str::from_utf8(&data[start..end]) {
                for line in text.lines().filter(|l| !l.is_empty()) {
                    if let Some(sep) = line.find(':') {
                        let key = line[..sep].trim().to_ascii_lowercase();
                        let value = line[sep + 1..].trim().to_string();
                        fields.insert(key, value);
                    }
                }
            }
        }
        index = end;
    }
    fields
}

// ── Heuristic protobuf scan ──

#[derive(Default)]
struct ProtobufScan {
    fixed32: Vec<Fixed32Field>,
    varint: Vec<VarintField>,
}

struct Fixed32Field {
    path: Vec<u64>,
    value: f32,
    order: usize,
}

struct VarintField {
    path: Vec<u64>,
    value: u64,
}

fn read_varint(bytes: &[u8], index: &mut usize) -> Option<u64> {
    let mut value: u64 = 0;
    let mut shift: u32 = 0;
    while *index < bytes.len() && shift < 64 {
        let byte = bytes[*index];
        *index += 1;
        value |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
    None
}

/// Recursively walk the protobuf wire format, recording varint + fixed32 fields
/// with their field-number path. `order` (a per-payload counter threaded
/// through recursion) preserves fixed32 read-order for tie-breaking.
fn scan_protobuf(
    bytes: &[u8],
    depth: u32,
    path: &[u64],
    order: usize,
    scan: &mut ProtobufScan,
) -> usize {
    let mut index = 0;
    let mut next_order = order;
    while index < bytes.len() {
        let field_start = index;
        let key = match read_varint(bytes, &mut index) {
            Some(k) if k != 0 => k,
            _ => {
                index = field_start + 1;
                continue;
            }
        };
        let field_number = key >> 3;
        let wire_type = key & 0x07;
        let mut field_path = path.to_vec();
        field_path.push(field_number);
        match wire_type {
            0 => match read_varint(bytes, &mut index) {
                Some(value) => scan.varint.push(VarintField {
                    path: field_path,
                    value,
                }),
                None => index = field_start + 1,
            },
            1 => {
                if index + 8 <= bytes.len() {
                    index += 8;
                } else {
                    return next_order;
                }
            }
            2 => {
                let length = match read_varint(bytes, &mut index) {
                    Some(l) if l <= (bytes.len() - index) as u64 => l as usize,
                    _ => {
                        index = field_start + 1;
                        continue;
                    }
                };
                let start = index;
                let end = index + length;
                if depth < 4 {
                    next_order =
                        scan_protobuf(&bytes[start..end], depth + 1, &field_path, next_order, scan);
                }
                index = end;
            }
            5 => {
                if index + 4 <= bytes.len() {
                    let bits = (bytes[index] as u32)
                        | ((bytes[index + 1] as u32) << 8)
                        | ((bytes[index + 2] as u32) << 16)
                        | ((bytes[index + 3] as u32) << 24);
                    scan.fixed32.push(Fixed32Field {
                        path: field_path,
                        value: f32::from_bits(bits),
                        order: next_order,
                    });
                    next_order += 1;
                    index += 4;
                } else {
                    return next_order;
                }
            }
            _ => index = field_start + 1,
        }
    }
    next_order
}

struct Snapshot {
    used_percent: Option<f64>,
    resets_at: Option<i64>,
}

fn parse_grpc_web_response(data: &[u8], now_ts: i64) -> Result<Snapshot, CollectorError> {
    let payloads = grpc_web_data_frames(data);
    if payloads.is_empty() {
        return Err(CollectorError::SchemaOrIo(
            "Grok: empty gRPC-web payload".to_string(),
        ));
    }
    let mut scan = ProtobufScan::default();
    for payload in &payloads {
        scan_protobuf(payload, 0, &[], 0, &mut scan);
    }

    // used-percent: the fixed32 (Float) whose path ends in `1`, value 0–100,
    // shallowest then earliest.
    let parsed_percent = scan
        .fixed32
        .iter()
        .filter(|f| {
            f.path.last() == Some(&1) && f.value.is_finite() && f.value >= 0.0 && f.value <= 100.0
        })
        .min_by(|a, b| {
            if a.path.len() == b.path.len() {
                a.order.cmp(&b.order)
            } else {
                a.path.len().cmp(&b.path.len())
            }
        })
        .map(|f| f.value as f64);

    // reset: a future unix-ts varint (seconds OR millis). Prefer path [1,5,1].
    let reset_fields: Vec<(Vec<u64>, i64)> = scan
        .varint
        .iter()
        .filter_map(|f| {
            let raw = f.value;
            if (1_700_000_000..=2_100_000_000).contains(&raw) {
                Some((f.path.clone(), raw as i64))
            } else if (1_700_000_000_000..=2_100_000_000_000).contains(&raw) {
                Some((f.path.clone(), (raw / 1000) as i64))
            } else {
                None
            }
        })
        .collect();
    let future: Vec<(Vec<u64>, i64)> = reset_fields
        .into_iter()
        .filter(|(_, ts)| *ts > now_ts)
        .collect();
    let reset = future
        .iter()
        .filter(|(p, _)| p.as_slice() == [1, 5, 1])
        .map(|(_, ts)| *ts)
        .min()
        .or_else(|| future.iter().map(|(_, ts)| *ts).min());

    // "No usage yet": reset present + a [1,6,…] varint, no fixed32 ⇒ 0% used.
    let no_usage_yet = parsed_percent.is_none()
        && scan.fixed32.is_empty()
        && reset.is_some()
        && scan.varint.iter().any(|f| f.path.starts_with(&[1, 6]));
    let percent = parsed_percent.or(if no_usage_yet { Some(0.0) } else { None });

    Ok(Snapshot {
        used_percent: percent,
        resets_at: reset,
    })
}

fn ts_iso(secs: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0).map(|d| d.to_rfc3339())
}

fn build_snapshot(s: &Snapshot) -> QuotaSnapshot {
    let reset = s.resets_at.and_then(ts_iso);
    match s.used_percent {
        Some(used) => {
            let clamped = used.clamp(0.0, 100.0);
            let remaining = (100.0 - clamped).round() as i64;
            QuotaSnapshot {
                plan_type: "Grok".to_string(),
                remaining,
                quota: 100,
                session_reset: reset.clone(),
                tiers: vec![TierEntry {
                    name: "Credits".to_string(),
                    quota: 100,
                    remaining,
                    reset_time: reset,
                }],
            }
        }
        None => QuotaSnapshot {
            // Auth succeeded but no usable usage field ⇒ status-only (no gauge).
            plan_type: "Grok".to_string(),
            remaining: 0,
            quota: 0,
            session_reset: reset,
            tiers: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_001;
    const FUTURE: u64 = 1_800_000_000; // in [1.7e9, 2.1e9], > NOW

    // ── protobuf / gRPC-web encoders ──
    fn varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut b = (v & 0x7F) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            out.push(b);
            if v == 0 {
                break;
            }
        }
        out
    }
    fn fixed32_field(field: u64, val: f32) -> Vec<u8> {
        let mut out = varint((field << 3) | 5);
        out.extend_from_slice(&val.to_bits().to_le_bytes());
        out
    }
    fn varint_field(field: u64, val: u64) -> Vec<u8> {
        let mut out = varint(field << 3);
        out.extend(varint(val));
        out
    }
    fn len_field(field: u64, inner: &[u8]) -> Vec<u8> {
        let mut out = varint((field << 3) | 2);
        out.extend(varint(inner.len() as u64));
        out.extend_from_slice(inner);
        out
    }
    fn data_frame(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x00];
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }
    fn trailer_frame(text: &str) -> Vec<u8> {
        let mut out = vec![0x80];
        out.extend_from_slice(&(text.len() as u32).to_be_bytes());
        out.extend_from_slice(text.as_bytes());
        out
    }

    #[test]
    fn extracts_used_percent_from_top_level_fixed32() {
        // field 1, fixed32 = 30.0 → path [1], last == 1 → used 30%.
        let frame = data_frame(&fixed32_field(1, 30.0));
        let snap = parse_grpc_web_response(&frame, NOW).unwrap();
        assert_eq!(snap.used_percent, Some(30.0));
    }

    #[test]
    fn extracts_reset_at_path_1_5_1() {
        // nested [1] → [5] → varint field 1 = FUTURE.
        let inner = len_field(5, &varint_field(1, FUTURE));
        let payload = len_field(1, &inner);
        let snap = parse_grpc_web_response(&data_frame(&payload), NOW).unwrap();
        assert_eq!(snap.resets_at, Some(FUTURE as i64));
        assert!(snap.used_percent.is_none()); // no fixed32
    }

    #[test]
    fn millis_reset_is_divided() {
        let ms = FUTURE * 1000; // in [1.7e12, 2.1e12]
        let payload = len_field(1, &len_field(5, &varint_field(1, ms)));
        let snap = parse_grpc_web_response(&data_frame(&payload), NOW).unwrap();
        assert_eq!(snap.resets_at, Some(FUTURE as i64));
    }

    #[test]
    fn no_usage_yet_is_zero_percent() {
        // varint at [1,6,1] + reset at [1,5,1], NO fixed32 → 0% used.
        let sub6 = len_field(6, &varint_field(1, 5));
        let sub5 = len_field(5, &varint_field(1, FUTURE));
        let mut inner = sub6;
        inner.extend(sub5);
        let payload = len_field(1, &inner);
        let snap = parse_grpc_web_response(&data_frame(&payload), NOW).unwrap();
        assert_eq!(snap.used_percent, Some(0.0));
        assert_eq!(snap.resets_at, Some(FUTURE as i64));
    }

    #[test]
    fn combined_percent_and_reset_maps_to_credits_tier() {
        let mut payload = fixed32_field(1, 30.0);
        payload.extend(len_field(1, &len_field(5, &varint_field(1, FUTURE))));
        let snap = parse_grpc_web_response(&data_frame(&payload), NOW).unwrap();
        let out = build_snapshot(&snap);
        assert_eq!(out.plan_type, "Grok");
        assert_eq!(out.quota, 100);
        assert_eq!(out.remaining, 70); // 100 - 30
        assert_eq!(out.tiers.len(), 1);
        assert_eq!(out.tiers[0].name, "Credits");
        assert_eq!(out.tiers[0].remaining, 70);
        assert!(out.session_reset.is_some());
    }

    #[test]
    fn no_percent_degrades_to_status_only_snapshot() {
        let snap = Snapshot {
            used_percent: None,
            resets_at: Some(FUTURE as i64),
        };
        let out = build_snapshot(&snap);
        assert_eq!(out.quota, 0);
        assert_eq!(out.remaining, 0);
        assert!(out.tiers.is_empty());
        assert!(out.session_reset.is_some());
    }

    #[test]
    fn data_frames_split_and_trailers_ignored() {
        let mut buf = data_frame(&fixed32_field(1, 10.0));
        buf.extend(trailer_frame("grpc-status:0\r\n"));
        let frames = grpc_web_data_frames(&buf);
        assert_eq!(frames.len(), 1); // trailer excluded
        let snap = parse_grpc_web_response(&buf, NOW).unwrap();
        assert_eq!(snap.used_percent, Some(10.0));
    }

    #[test]
    fn grpc_status_error_is_rejected() {
        // header field
        let mut h = HashMap::new();
        h.insert("grpc-status".to_string(), "16".to_string());
        assert!(validate_grpc_status(&h).is_err());
        // trailer frame
        let tr = trailer_frame("grpc-status:5\r\ngrpc-message:boom\r\n");
        assert!(validate_grpc_status(&grpc_web_trailer_fields(&tr)).is_err());
        // status 0 / absent → ok
        let mut ok = HashMap::new();
        ok.insert("grpc-status".to_string(), "0".to_string());
        assert!(validate_grpc_status(&ok).is_ok());
        assert!(validate_grpc_status(&HashMap::new()).is_ok());
    }

    #[test]
    fn empty_payload_is_error() {
        // only a trailer, no data frame.
        let tr = trailer_frame("grpc-status:0\r\n");
        assert!(parse_grpc_web_response(&tr, NOW).is_err());
    }
}
