//! Wire-format helpers for the platform network tasks (radar-ui RD.8): SNTP and HTTP/1.1 replies.
//! Pure functions, host-tested; the sockets live in the firmware.

use alloc::vec::Vec;

/// SNTP client request: LI 0, version 3, mode 3 (client), the rest zero.
pub const SNTP_REQUEST: [u8; 48] = {
    let mut p = [0; 48];
    p[0] = 0x1B;
    p
};

const NTP_TO_UNIX_S: u64 = 2_208_988_800;
/// 2025-01-01: an earlier reply is a broken or spoofed clock, not a time to validate certificates with.
const MIN_UNIX_S: u64 = 1_735_689_600;

/// Unix seconds from an SNTP reply (transmit timestamp). `None` for a short packet, a reply that is
/// not mode 4 (server), stratum 0 (kiss-o'-death) or an implausible time.
pub fn parse_sntp(reply: &[u8]) -> Option<u64> {
    let r: &[u8; 48] = reply.get(..48)?.try_into().ok()?;
    if r[0] & 7 != 4 || r[1] == 0 {
        return None;
    }
    let ntp = u64::from(u32::from_be_bytes([r[40], r[41], r[42], r[43]]));
    if ntp == 0 {
        return None; // unsynchronised server
    }
    // The 32-bit NTP seconds wrap in 2036; treat small values as the next era.
    let unix = if ntp >= NTP_TO_UNIX_S { ntp - NTP_TO_UNIX_S } else { ntp + (1 << 32) - NTP_TO_UNIX_S };
    (unix >= MIN_UNIX_S).then_some(unix)
}

/// Unix seconds to (year, month, day, hour, minute, second) in UTC, for log lines.
pub fn utc(unix: u64) -> (u64, u64, u64, u64, u64, u64) {
    let (days, rem) = (unix / 86_400, unix % 86_400);
    // Civil-from-days (Howard Hinnant), shifted to start the year on 1 March.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day, rem / 3600, rem % 3600 / 60, rem % 60)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub status: u16,
    /// Offset of the first body byte.
    pub body_start: usize,
    pub chunked: bool,
    pub content_length: Option<usize>,
    /// The server said `Connection: close`.
    pub close: bool,
}

/// Parses the status line and headers. `None` while the head is incomplete or malformed.
pub fn parse_head(buf: &[u8]) -> Option<Head> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = core::str::from_utf8(&buf[..end]).ok()?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next()?;
    let status = status_line.strip_prefix("HTTP/1.")?.get(2..5)?.parse().ok()?;
    let (mut chunked, mut content_length, mut close) = (false, None, false);
    for (k, v) in lines.filter_map(|l| l.split_once(':')) {
        let (k, v) = (k.trim(), v.trim().to_ascii_lowercase());
        if k.eq_ignore_ascii_case("transfer-encoding") {
            chunked |= v.contains("chunked");
        } else if k.eq_ignore_ascii_case("content-length") {
            content_length = v.parse().ok();
        } else if k.eq_ignore_ascii_case("connection") {
            close |= v.contains("close");
        }
    }
    Some(Head { status, body_start: end + 4, chunked, content_length, close })
}

/// Decodes a chunked body. The flag is false when the input ends before the last chunk
/// (the decoded prefix is still returned).
pub fn dechunk(mut body: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    loop {
        let Some(eol) = body.windows(2).position(|w| w == b"\r\n") else {
            return (out, false);
        };
        let size = core::str::from_utf8(&body[..eol])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next()?.trim(), 16).ok());
        let Some(size) = size else {
            return (out, false);
        };
        body = &body[eol + 2..];
        if size == 0 {
            return (out, true);
        }
        let take = size.min(body.len());
        out.extend_from_slice(&body[..take]);
        if take < size {
            return (out, false);
        }
        body = body.get(size + 2..).unwrap_or(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(mode: u8, stratum: u8, ntp: u32) -> [u8; 48] {
        let mut r = [0; 48];
        r[0] = 0x1C & !7 | mode;
        r[1] = stratum;
        r[40..44].copy_from_slice(&ntp.to_be_bytes());
        r
    }

    #[test]
    fn sntp_ok_and_rejects() {
        // 2026-10-23 00:00:00 UTC
        let unix = 1_792_713_600u64;
        let ntp = (unix + NTP_TO_UNIX_S) as u32;
        assert_eq!(parse_sntp(&reply(4, 2, ntp)), Some(unix));
        assert_eq!(parse_sntp(&reply(3, 2, ntp)), None, "client mode");
        assert_eq!(parse_sntp(&reply(4, 0, ntp)), None, "kiss-o'-death");
        assert_eq!(parse_sntp(&reply(4, 2, 0)), None, "zero time");
        assert_eq!(parse_sntp(&reply(4, 2, (NTP_TO_UNIX_S + 1_000) as u32)), None, "1970");
        assert_eq!(parse_sntp(&[0x24; 47]), None, "short");
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc(1_792_713_600 + 3661), (2026, 10, 23, 1, 1, 1));
        assert_eq!(utc(1_709_164_800), (2024, 2, 29, 0, 0, 0)); // leap day
        assert_eq!(utc(2_114_380_800), (2037, 1, 1, 0, 0, 0));
    }

    #[test]
    fn sntp_era_rollover() {
        // 2037-01-01 = 2_114_380_800 unix; NTP seconds wrapped past 2^32.
        let unix = 2_114_380_800u64;
        let ntp = (unix + NTP_TO_UNIX_S - (1 << 32)) as u32;
        assert_eq!(parse_sntp(&reply(4, 2, ntp)), Some(unix));
    }

    #[test]
    fn head_status_and_chunked() {
        let h = parse_head(b"HTTP/1.1 206 Partial\r\nContent-Length: 4\r\nTransfer-Encoding: Chunked\r\n\r\nabcd").unwrap();
        assert_eq!((h.status, h.chunked, h.body_start), (206, true, 71));
        let h = parse_head(b"HTTP/1.0 403 Forbidden\r\nServer: x\r\n\r\n").unwrap();
        assert_eq!((h.status, h.chunked, h.content_length, h.close), (403, false, None, false));
        let h = parse_head(b"HTTP/1.1 200 OK\r\ncontent-length: 454\r\nConnection: Close\r\n\r\n").unwrap();
        assert_eq!((h.content_length, h.close), (Some(454), true));
        let h = parse_head(b"HTTP/1.1 200 OK\r\nContent-Length: x\r\nConnection: keep-alive\r\n\r\n").unwrap();
        assert_eq!((h.content_length, h.close), (None, false));
        assert_eq!(parse_head(b"HTTP/1.1 200 OK\r\nServer: x\r\n"), None, "incomplete");
        assert_eq!(parse_head(b"garbage\r\n\r\n"), None);
        assert_eq!(parse_head(b"HTTP/1.1 2x0 OK\r\n\r\n"), None);
    }

    #[test]
    fn chunks() {
        assert_eq!(dechunk(b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n"), (b"hello world".to_vec(), true));
        assert_eq!(dechunk(b"5\r\nhel"), (b"hel".to_vec(), false));
        assert_eq!(dechunk(b"5\r\nhello\r\n"), (b"hello".to_vec(), false));
        assert_eq!(dechunk(b"zz\r\nhello"), (Vec::new(), false));
        assert_eq!(dechunk(b""), (Vec::new(), false));
    }
}
