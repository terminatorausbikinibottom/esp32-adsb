# tls-hostile

Host-only regression test for the firmware's TLS verification (radar-ui RD.8). It runs
`adsb_core::tlsauth::open_verified` (the exact code the firmware uses) against `srv.py`, a minimal
TLS 1.3 server that can leave out or forge the authentication messages: no Certificate, a real chain
without CertificateVerify (embedded-tls 0.19 accepts both without the `tlsauth` guard), forged chains,
a leaf signed by an ordinary leaf, plus the good flow and an unsynced clock.

Needs `openssl` and Python 3 with `cryptography`:

    python3 -m venv .venv && .venv/bin/pip install cryptography
    TLS_HOSTILE_PYTHON=$PWD/.venv/bin/python cargo test

The test PKI (fictitious names, throw-away keys) is generated into `target/fixtures` by `gen.sh`;
nothing is committed. Not part of the xtensa build (own workspace).
