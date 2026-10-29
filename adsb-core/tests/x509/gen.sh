#!/bin/sh
# Regenerates the DER fixtures (openssl 3). Names are fictitious; keys are discarded.
set -e
cd "$(dirname "$0")"
T=$(mktemp -d); trap 'rm -rf $T' EXIT
key() { openssl ecparam -name prime256v1 -genkey -noout -out $T/$1.key; }
# mk <name> <subject CN> <issuer name> <exts> : self-signed when issuer == name
mk() {
  n=$1; cn=$2; iss=$3; ext=$4
  [ -f $T/$n.key ] || key $n
  printf '%s\n' "$ext" > $T/$n.ext
  openssl req -new -key $T/$n.key -subj "/CN=$cn" -out $T/$n.csr
  if [ "$iss" = "$n" ]; then
    openssl x509 -req -in $T/$n.csr -signkey $T/$n.key -days 36500 -extfile $T/$n.ext -out $T/$n.pem 2>/dev/null
  else
    openssl x509 -req -in $T/$n.csr -CA $T/$iss.pem -CAkey $T/$iss.key -CAcreateserial -days 36500 -extfile $T/$n.ext -out $T/$n.pem 2>/dev/null
  fi
  openssl x509 -in $T/$n.pem -outform DER -out $n.der
}
CA='basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign,cRLSign'
LEAF='basicConstraints=CA:FALSE
keyUsage=digitalSignature
extendedKeyUsage=serverAuth'
mk root "Test Root" root "$CA"
mk otherroot "Other Root" otherroot "$CA"
mk inter "Test Inter" root "$CA"
mk leaf "victim.test" inter "$LEAF"
mk leaf2 "victim.test" root "$LEAF"
mk forged "victim.test" leaf "$LEAF"
mk nobc "No BC Inter" root "keyUsage=keyCertSign"
mk caleaf "ca.test" inter "$CA"
mk kuleaf "ku.test" inter "basicConstraints=CA:FALSE
keyUsage=keyEncipherment"
mk ekuleaf "eku.test" inter "basicConstraints=CA:FALSE
extendedKeyUsage=clientAuth"
mk kuinter "Bad KU Inter" root "basicConstraints=critical,CA:TRUE
keyUsage=digitalSignature"
mk leaf3 "l3.test" kuinter "$LEAF"
mk inter0 "Inter Zero" root "basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=keyCertSign"
mk inter1 "Inter One" inter0 "$CA"
mk leaf4 "l4.test" inter1 "$LEAF"
