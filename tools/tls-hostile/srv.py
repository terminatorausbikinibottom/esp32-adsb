import sys, socket, struct, hmac, hashlib, os, time
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives import serialization, hashes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
mode, port = sys.argv[1], int(sys.argv[2]); fx = sys.argv[3]
def hkdf_extract(salt, ikm): return hmac.new(salt, ikm, hashlib.sha256).digest()
def expand_label(secret, label, ctx, n):
    full = b"tls13 " + label
    info = struct.pack(">H", n) + bytes([len(full)]) + full + bytes([len(ctx)]) + ctx
    out, t, i = b"", b"", 1
    while len(out) < n:
        t = hmac.new(secret, t + info + bytes([i]), hashlib.sha256).digest(); out += t; i += 1
    return out[:n]
H = lambda b: hashlib.sha256(b).digest()
def derive(secret, label, transcript): return expand_label(secret, label, H(transcript), 32)
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", port)); s.listen(1)
c, _ = s.accept()
def rd(n):
    b = b""
    while len(b) < n:
        d = c.recv(n - len(b));
        if not d: raise EOFError
        b += d
    return b
typ, ver, ln = struct.unpack(">BHH", rd(5)); ch = rd(ln)   # ClientHello handshake msg
assert ch[0] == 1
p = 4 + 2 + 32; sidlen = ch[p]; sid = ch[p+1:p+1+sidlen]; p += 1 + sidlen
cslen = struct.unpack(">H", ch[p:p+2])[0]; p += 2 + cslen
p += 1 + ch[p]
extlen = struct.unpack(">H", ch[p:p+2])[0]; p += 2; end = p + extlen; peer = None
while p < end:
    et, el = struct.unpack(">HH", ch[p:p+4]); body = ch[p+4:p+4+el]; p += 4 + el
    if et == 0x33:
        q = 2
        while q < len(body):
            g, kl = struct.unpack(">HH", body[q:q+4]); k = body[q+4:q+4+kl]; q += 4 + kl
            if g == 0x17: peer = k
assert peer, "no secp256r1 share"
key = ec.generate_private_key(ec.SECP256R1())
pub = key.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
shared = key.exchange(ec.ECDH(), ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), peer))
ext = struct.pack(">HHH", 0x2b, 2, 0x0304) + struct.pack(">HHHH", 0x33, 4 + 65, 0x17, 65) + pub
sh = b"\x03\x03" + os.urandom(32) + bytes([len(sid)]) + sid + b"\x13\x01\x00" + struct.pack(">H", len(ext)) + ext
shm = b"\x02" + struct.pack(">I", len(sh))[1:] + sh
c.sendall(b"\x16\x03\x03" + struct.pack(">H", len(shm)) + shm)
tr = ch + shm
early = hkdf_extract(b"\x00"*32, b"\x00"*32)
hs = hkdf_extract(derive(early, b"derived", b""), shared)
s_hs = derive(hs, b"s hs traffic", tr)
k = expand_label(s_hs, b"key", b"", 16); iv = expand_label(s_hs, b"iv", b"", 12); seq = 0
def send_hs(msg):
    global seq, tr
    tr += msg
    inner = msg + b"\x16"
    hdr = b"\x17\x03\x03" + struct.pack(">H", len(inner) + 16)
    nonce = bytes(a ^ b for a, b in zip(iv, b"\x00"*4 + struct.pack(">Q", seq))); seq += 1
    c.sendall(hdr + AESGCM(k).encrypt(nonce, inner, hdr))
hsmsg = lambda t, b: bytes([t]) + struct.pack(">I", len(b))[1:] + b
send_hs(hsmsg(8, b"\x00\x00"))
# modes: nocert (no Certificate / CertificateVerify), nocv (real chain, no proof of the key),
# nocv_bad (forged chain, no proof), good (real chain + valid CertificateVerify),
# forged (attacker chain + CertificateVerify with the attacker key),
# leafsigned (a leaf-signed leaf + valid CertificateVerify), dupcv (good flow, CertificateVerify sent twice)
CHAINS = {"nocv": ("leaf", "inter"), "nocv_bad": ("badleaf", "attinter"), "good": ("leaf", "inter"),
          "forged": ("badleaf", "attinter"), "dupcv": ("leaf", "inter"), "leafsigned": ("forged", "leaf", "inter")}
if mode in CHAINS:
    names = CHAINS[mode]
    chain = [open(f"{fx}/{n}.der", "rb").read() for n in names]
    body = b"\x00" + struct.pack(">I", sum(3 + len(x) + 2 for x in chain))[1:] + b"".join(struct.pack(">I", len(x))[1:] + x + b"\x00\x00" for x in chain)
    send_hs(hsmsg(11, body))
    if mode in ("good", "forged", "leafsigned", "dupcv"):
        k_leaf = serialization.load_pem_private_key(open(f"{fx}/{names[0]}.key", "rb").read(), None)
        content = b"\x20" * 64 + b"TLS 1.3, server CertificateVerify\x00" + H(tr)
        sig = k_leaf.sign(content, ec.ECDSA(hashes.SHA256()))
        send_hs(hsmsg(15, struct.pack(">HH", 0x0403, len(sig)) + sig))
        if mode == "dupcv":
            send_hs(hsmsg(15, struct.pack(">HH", 0x0403, len(sig)) + sig))
fin = hmac.new(expand_label(s_hs, b"finished", b"", 32), H(tr), hashlib.sha256).digest()
send_hs(hsmsg(20, fin))
c.settimeout(3)
try:
    while True:
        d = c.recv(4096)
        if not d: break
except Exception: pass
