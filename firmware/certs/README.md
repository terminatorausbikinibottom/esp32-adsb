# Pinned root certificates (public)

DER files compiled into the firmware by `src/tls.rs` for the metadata hosts. They are public CA
certificates (taken from the Mozilla CA bundle, `ca-certificates` package, 2026-10-23), safe to commit.
Verification is always on; a host is only reachable if the last certificate of its server chain is
signed by the pinned root (a changed chain fails closed and needs a reflash).

| File | Subject | Expires | SHA-256 fingerprint | Used for |
|------|---------|---------|---------------------|----------|
| `isrg-root-x1.der` | ISRG Root X1 (Internet Security Research Group) | 2035-06-04 | `96:BC:EC:06:26:49:76:F3:74:60:77:9A:CF:28:C5:A7:CF:E8:A3:C0:AA:E1:1A:8F:FC:EE:05:C0:BD:DF:08:C6` | `api.adsbdb.com` (cross-signed ISRG Root X2 is the last chain entry), `t.plnspttrs.net` (X1 itself is sent) |
| `globalsign-root-ca.der` | GlobalSign Root CA | 2028-01-28 | `EB:D4:10:40:E4:BB:3E:C7:42:C9:E3:81:D3:1E:F2:A4:1A:48:B6:68:5C:96:E7:CE:F3:C1:DF:6C:D4:33:1C:99` | `api.planespotters.net` (GTS Root R4, cross-signed by this root, is the last chain entry) |

The GlobalSign root expires first (2028-01-28): plan a reflash before then, or earlier if
`api.planespotters.net` stops sending the GlobalSign cross-signed chain.
