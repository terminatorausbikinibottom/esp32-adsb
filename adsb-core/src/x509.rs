//! Structural checks of a server certificate chain that signature verification alone does not give
//! (radar-ui RD.8 fix): CA flags, key usage, path length and issuer/subject chaining. Without them
//! any holder of an ordinary leaf certificate could sign a forged leaf for another host.
//! The signatures, validity time and host name are checked by the TLS stack; this is the rest.
//! A minimal DER reader: every parse problem rejects (fail closed).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    Malformed,
    LeafIsCa,
    LeafKeyUsage,
    LeafExtKeyUsage,
    NotCa,
    CaKeyUsage,
    PathLen,
    IssuerMismatch,
    EmptyChain,
}

struct Info<'a> {
    issuer: &'a [u8],
    subject: &'a [u8],
    /// `Some((cA, pathLen))` when basicConstraints is present.
    basic: Option<(bool, Option<u32>)>,
    /// First keyUsage byte (digitalSignature 0x80, keyCertSign 0x04).
    key_usage: Option<u8>,
    /// serverAuth or anyExtendedKeyUsage listed (`None`: no EKU extension).
    eku_server: Option<bool>,
}

/// (tag, whole TLV, content, rest)
type Tlv<'a> = (u8, &'a [u8], &'a [u8], &'a [u8]);

/// Reads one TLV.
fn tlv(b: &[u8]) -> Option<Tlv<'_>> {
    let (&tag, r) = b.split_first()?;
    let (&l0, r) = r.split_first()?;
    let (len, hdr) = if l0 < 0x80 {
        (usize::from(l0), 2)
    } else {
        let n = usize::from(l0 & 0x7F);
        if n == 0 || n > 3 || r.len() < n {
            return None;
        }
        (r[..n].iter().fold(0usize, |a, &x| a << 8 | usize::from(x)), 2 + n)
    };
    let end = hdr.checked_add(len)?;
    (b.len() >= end).then(|| (tag, &b[..end], &b[hdr..end], &b[end..]))
}

fn parse(der: &[u8]) -> Option<Info<'_>> {
    let (0x30, _, cert, _) = tlv(der)? else { return None };
    let (0x30, _, mut t, _) = tlv(cert)? else { return None };
    if t.first() == Some(&0xA0) {
        t = tlv(t)?.3; // version
    }
    t = tlv(t)?.3; // serial
    t = tlv(t)?.3; // signature algorithm
    let (_, issuer, _, r) = tlv(t)?;
    t = tlv(r)?.3; // validity
    let (_, subject, _, r) = tlv(t)?;
    t = tlv(r)?.3; // subject public key info
    let mut info = Info { issuer, subject, basic: None, key_usage: None, eku_server: None };
    while !t.is_empty() {
        let (tag, _, content, rest) = tlv(t)?;
        t = rest;
        if tag != 0xA3 {
            continue; // unique ids
        }
        let (0x30, _, mut exts, _) = tlv(content)? else { return None };
        while !exts.is_empty() {
            let (0x30, _, mut e, rest) = tlv(exts)? else { return None };
            exts = rest;
            let (0x06, _, oid, r) = tlv(e)? else { return None };
            e = r;
            if let Some((0x01, _, _, r)) = tlv(e) {
                e = r; // critical flag
            }
            let (0x04, _, value, _) = tlv(e)? else { return None };
            match oid {
                [0x55, 0x1D, 0x13] => {
                    let (0x30, _, mut s, _) = tlv(value)? else { return None };
                    let mut ca = false;
                    if let Some((0x01, _, c, r)) = tlv(s) {
                        ca = c.first()? != &0;
                        s = r;
                    }
                    let path_len = match tlv(s) {
                        Some((0x02, _, [n], _)) => Some(u32::from(*n)),
                        Some(_) => return None,
                        None => None,
                    };
                    info.basic = Some((ca, path_len));
                }
                [0x55, 0x1D, 0x0F] => {
                    let (0x03, _, bits, _) = tlv(value)? else { return None };
                    info.key_usage = Some(*bits.get(1)?);
                }
                [0x55, 0x1D, 0x25] => {
                    let (0x30, _, mut s, _) = tlv(value)? else { return None };
                    let mut ok = false;
                    while !s.is_empty() {
                        let (0x06, _, o, r) = tlv(s)? else { return None };
                        ok |= o == [0x2B, 6, 1, 5, 5, 7, 3, 1] || o == [0x55, 0x1D, 0x25, 0];
                        s = r;
                    }
                    info.eku_server = Some(ok);
                }
                _ => {}
            }
        }
    }
    Some(info)
}

/// Checks the chain as sent by the server (`chain[0]` = leaf) against the pinned root's DER.
pub fn check_chain(chain: &[&[u8]], root: &[u8]) -> Result<(), Reject> {
    if chain.len() > 8 {
        return Err(Reject::Malformed); // longer than any real chain; also bounds the stack buffer
    }
    let certs: heapless::Vec<Info, 8> = chain
        .iter()
        .map(|c| parse(c).ok_or(Reject::Malformed))
        .collect::<Result<_, _>>()?;
    let root = parse(root).ok_or(Reject::Malformed)?;
    let leaf = certs.first().ok_or(Reject::EmptyChain)?;
    if leaf.basic.is_some_and(|(ca, _)| ca) {
        return Err(Reject::LeafIsCa);
    }
    if leaf.key_usage.is_some_and(|k| k & 0x80 == 0) {
        return Err(Reject::LeafKeyUsage);
    }
    if leaf.eku_server == Some(false) {
        return Err(Reject::LeafExtKeyUsage);
    }
    for (i, c) in certs.iter().enumerate().skip(1) {
        let Some((true, path_len)) = c.basic else {
            return Err(Reject::NotCa);
        };
        if c.key_usage.is_some_and(|k| k & 0x04 == 0) {
            return Err(Reject::CaKeyUsage);
        }
        // Certificates between this one and the leaf (self-issued ones are not special-cased: stricter).
        if path_len.is_some_and(|p| (i - 1) as u32 > p) {
            return Err(Reject::PathLen);
        }
    }
    for (c, issuer) in certs.iter().zip(certs.iter().skip(1).map(|n| n.subject).chain([root.subject])) {
        if c.issuer != issuer {
            return Err(Reject::IssuerMismatch);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! der {
        ($n:literal) => {
            &include_bytes!(concat!("../tests/x509/", $n, ".der"))[..]
        };
    }

    #[test]
    fn good_chain_passes() {
        assert_eq!(check_chain(&[der!("leaf"), der!("inter"), der!("root")], der!("root")), Ok(()));
        assert_eq!(check_chain(&[der!("leaf"), der!("inter")], der!("root")), Ok(()));
    }

    #[test]
    fn forged_leaf_under_a_leaf_fails() {
        // forged leaf signed by an ordinary leaf: [forged, leaf, inter]
        assert_eq!(check_chain(&[der!("forged"), der!("leaf"), der!("inter")], der!("root")), Err(Reject::NotCa));
    }

    #[test]
    fn intermediate_without_basic_constraints_fails() {
        assert_eq!(check_chain(&[der!("leaf2"), der!("nobc")], der!("root")), Err(Reject::NotCa));
    }

    #[test]
    fn ca_leaf_fails() {
        assert_eq!(check_chain(&[der!("caleaf"), der!("inter")], der!("root")), Err(Reject::LeafIsCa));
    }

    #[test]
    fn key_usage_rules() {
        assert_eq!(check_chain(&[der!("kuleaf"), der!("inter")], der!("root")), Err(Reject::LeafKeyUsage));
        assert_eq!(check_chain(&[der!("ekuleaf"), der!("inter")], der!("root")), Err(Reject::LeafExtKeyUsage));
        assert_eq!(check_chain(&[der!("leaf3"), der!("kuinter")], der!("root")), Err(Reject::CaKeyUsage));
    }

    #[test]
    fn path_length_enforced() {
        // inter0 has pathLen 0 but sits above another CA
        assert_eq!(check_chain(&[der!("leaf4"), der!("inter1"), der!("inter0")], der!("root")), Err(Reject::PathLen));
    }

    #[test]
    fn issuer_must_chain_and_end_at_the_root() {
        assert_eq!(check_chain(&[der!("leaf"), der!("inter")], der!("otherroot")), Err(Reject::IssuerMismatch));
        assert_eq!(check_chain(&[der!("leaf"), der!("root")], der!("root")), Err(Reject::IssuerMismatch));
    }

    #[test]
    fn garbage_and_empty_reject() {
        assert_eq!(check_chain(&[&[0x30, 0x03, 1, 2, 3]], der!("root")), Err(Reject::Malformed));
        assert_eq!(check_chain(&[der!("leaf")], &[]), Err(Reject::Malformed));
        assert_eq!(check_chain(&[], der!("root")), Err(Reject::EmptyChain));
        assert_eq!(check_chain(&[der!("leaf"); 9], der!("root")), Err(Reject::Malformed));
    }
}
