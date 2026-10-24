#!/bin/sh
# Usage: gen.sh <outdir>. Fictitious test PKI (throw-away keys, never committed): the real chain
# root > inter > leaf (victim.test), an attacker chain with the same DNs under another root, and a
# leaf-signed "forged" leaf (attacker holds an ordinary leaf and signs a certificate for victim.test).
set -e
mkdir -p "$1"; cd "$1"
mk(){ n=$1; cn=$2; ca=$3; ext=$4
 [ -f $n.key ] || openssl ecparam -name prime256v1 -genkey -noout -out $n.key
 printf '%s\n' "$ext" > $n.ext
 openssl req -new -key $n.key -subj "/CN=$cn" -out $n.csr
 if [ "$ca" = self ]; then openssl x509 -req -in $n.csr -signkey $n.key -days 3650 -extfile $n.ext -out $n.pem 2>/dev/null
 else openssl x509 -req -in $n.csr -CA $ca.pem -CAkey $ca.key -CAcreateserial -days 3650 -extfile $n.ext -out $n.pem 2>/dev/null; fi
 openssl x509 -in $n.pem -outform DER -out $n.der; }
CA='basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign'
LEAF='basicConstraints=CA:FALSE
keyUsage=digitalSignature
extendedKeyUsage=serverAuth
subjectAltName=DNS:victim.test'
mk root "Test Root" self "$CA"
mk inter "Test Inter" root "$CA"
mk leaf "victim.test" inter "$LEAF"
mk attroot "Test Root" self "$CA"
mk attinter "Test Inter" attroot "$CA"
mk badleaf "victim.test" attinter "$LEAF"
mk forged "victim.test" leaf "$LEAF"
