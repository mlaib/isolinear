//! Security, software and network files recognized by `inspect`: X.509, PEM, CMS, PKCS#12,
//! JWT, generic DER, WebAssembly, Java classes, ELF, PE and packet captures.
//!
//! Fixtures in tests/fixtures/standards were made with throwaway keys (worthless, generated
//! for these tests only) by OpenSSL 3.0.13, gcc and python3 -I (standard library only):
//!
//! ```sh
//! # ca.cnf: [dn] C=LU, O=Isolinear Test, CN=Isolinear Test CA;
//! #   [ca_ext] basicConstraints=critical,CA:TRUE,pathlen:0; keyUsage=critical,keyCertSign,cRLSign;
//! #            subjectKeyIdentifier=hash
//! #   [leaf_ext] basicConstraints=CA:FALSE; keyUsage=critical,digitalSignature,nonRepudiation;
//! #            extendedKeyUsage=serverAuth,clientAuth,emailProtection;
//! #            subjectAltName=DNS:test.example,DNS:www.test.example,IP:192.0.2.7,email:signer@test.example;
//! #            subjectKeyIdentifier=hash; authorityKeyIdentifier=keyid;
//! #            crlDistributionPoints=URI:http://crl.test.example/ca.crl;
//! #            authorityInfoAccess=OCSP;URI:http://ocsp.test.example,caIssuers;URI:http://ca.test.example/ca.cer
//! openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.pem -days 3650 -config ca.cnf -extensions ca_ext -set_serial 0x1001
//! openssl ecparam -name prime256v1 -genkey -noout | openssl pkcs8 -topk8 -nocrypt -out leaf.key
//! openssl req -new -key leaf.key -subj "/C=LU/L=Esch-sur-Alzette/O=Isolinear Test/CN=test.example" -addext "subjectAltName=DNS:test.example" -out req.pem
//! openssl x509 -req -in req.pem -CA ca.pem -CAkey ca.key -set_serial 0x0123456789abcdef01 -days 3650 -extfile ca.cnf -extensions leaf_ext -out leaf.pem
//! openssl x509 -in leaf.pem -outform DER -out leaf.der                       # x509-leaf.der
//! cat leaf.pem ca.pem > chain.pem                                            # x509-chain.pem; ca.pem is x509-ca.crt
//! openssl pkey -in leaf.key -pubout -out leaf.pub.pem                        # ec-public.pem; req.pem is csr.pem
//! openssl ec -in leaf.key -out leaf-sec1.key                                 # throwaway-ec-sec1.key; leaf.key is throwaway-ec-key.pem
//! printf 'Isolinear signed content.\n' > content.txt                         # cms-content.txt
//! openssl cms -sign -in content.txt -signer leaf.pem -inkey leaf.key -certfile ca.pem -outform DER -out signed.p7m -nodetach -cades -binary -md sha256
//! openssl cms -sign -in content.txt -signer leaf.pem -inkey leaf.key -outform DER -out detached.p7s -cades -binary -md sha256
//! openssl crl2pkcs7 -nocrl -certfile chain.pem -outform DER -out certs.p7b
//! openssl pkcs12 -export -in leaf.pem -inkey leaf.key -certfile ca.pem -name "Test leaf" -passout pass:throwaway -out store.p12
//! openssl pkcs12 -export -in leaf.pem -inkey leaf.key -name "Plain" -certpbe NONE -keypbe NONE -passout pass:throwaway -out plain.p12
//! # token.jwt: HS256 over {"alg":"HS256","typ":"JWT","kid":"test-key-1"} and
//! #   {"iss":"https://issuer.test.example","sub":"user-42","aud":["api","web"],"iat":1767225600,
//! #    "nbf":1767225600,"exp":4102444800,"name":"Ada"} with the key "throwaway" (python3 -I, hmac).
//! # elf-demo: gcc -Os -o elf-demo demo.c -Wl,--build-id=sha1 -Wl,-rpath,/opt/isolinear/lib -Wl,--enable-new-dtags
//! #   (demo.c: int isolinear_answer(void) { return 42; } and a main printing it)
//! # elf-libdemo.so: gcc -Os -shared -fPIC -o elf-libdemo.so lib.c -Wl,-soname,libdemo.so.1 -Wl,--build-id=sha1
//! #   (lib.c: int isolinear_lib(int x) { return x * 2; })
//! ```
//! WebAssembly, Java class, PE and packet capture inputs are assembled byte by byte below.
use isolinear_formats::{
    detect,
    inspect::{
        Input, Inspection, MAX_ROWS, Stage, State, inspect_file, probe, recognize,
        standards::hash::{hex, sha1, sha256},
    },
};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/standards")
        .join(name)
}

fn inspect(path: &Path) -> Option<Result<Inspection, String>> {
    let head = std::fs::read(path).unwrap();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, &head[..head.len().min(65536)]).format.id;
    inspect_file(path, format, &|| false)
}

fn open(name: &str) -> Inspection {
    inspect(&fixture(name))
        .unwrap_or_else(|| panic!("{name} not recognized"))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Inspects bytes as if they were a file called `name`, calling the handler directly (no
/// panic guard), so a panic fails the test. `None` when no handler claims them.
fn run(name: &str, bytes: &[u8]) -> Option<Result<Inspection, String>> {
    let path = Path::new(name);
    let head = &bytes[..bytes.len().min(65536)];
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let format = detect(&name, head).format.id;
    let p = probe(path, head, bytes.len() as u64, format);
    let h = recognize(&p)?;
    let input = Input {
        path,
        bytes,
        complete: true,
        size: bytes.len() as u64,
    };
    Some(h.inspect(&input, &|| false))
}

fn built(name: &str, bytes: &[u8]) -> Inspection {
    run(name, bytes)
        .unwrap_or_else(|| panic!("{name} not recognized"))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn fact<'a>(i: &'a Inspection, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {label:?} in {:#?}", i.facts))
}

fn fact_in<'a>(i: &'a Inspection, group: &str, label: &str) -> &'a str {
    i.facts
        .iter()
        .find(|f| f.group == group && f.label == label)
        .map(|f| f.value.as_str())
        .unwrap_or_else(|| panic!("no fact {group:?}/{label:?} in {:#?}", i.facts))
}

fn check(i: &Inspection, stage: Stage, contains: &str) -> State {
    i.checks
        .iter()
        .find(|c| c.stage == stage && c.detail.contains(contains))
        .unwrap_or_else(|| panic!("no {stage:?} check {contains:?} in {:#?}", i.checks))
        .state
}

fn table<'a>(i: &'a Inspection, name: &str) -> &'a isolinear_formats::inspect::Table {
    i.tables.iter().find(|t| t.name == name).unwrap_or_else(|| {
        panic!(
            "no table {name:?} in {:?}",
            i.tables.iter().map(|t| &t.name).collect::<Vec<_>>()
        )
    })
}

fn cell<'a>(t: &'a isolinear_formats::inspect::Table, row: usize, column: &str) -> &'a str {
    let c = t
        .columns
        .iter()
        .position(|c| c == column)
        .unwrap_or_else(|| panic!("no column {column:?} in {:?}", t.columns));
    &t.rows[row].cells[c]
}

/// Every piece of text an inspection shows.
fn all_text(i: &Inspection) -> Vec<String> {
    let mut v = vec![i.family.clone(), i.kind.clone()];
    v.extend(i.version.clone());
    v.extend(i.checks.iter().map(|c| c.detail.clone()));
    v.extend(i.notes.iter().cloned());
    for f in &i.facts {
        v.push(f.label.clone());
        v.push(f.value.clone());
    }
    for t in &i.tables {
        for r in &t.rows {
            v.extend(r.cells.iter().cloned());
        }
    }
    for n in &i.outline {
        v.push(n.label.clone());
        v.push(n.value.clone());
    }
    v
}

fn pem_body(text: &str) -> Vec<u8> {
    let b64: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
    // Small base64 decoder for the test (standard alphabet).
    let mut out = vec![];
    let (mut acc, mut bits) = (0u32, 0);
    for c in b64.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

// ---------------------------------------------------------------------------------------------
// Hashes

#[test]
fn sha1_and_sha256_known_answers() {
    let cases: &[(&[u8], &str, &str)] = &[
        (
            b"",
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc",
            "a9993e364706816aba3e25717850c26c9cd0d89d",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ];
    for (input, s1, s256) in cases {
        assert_eq!(hex(&sha1(input)), *s1);
        assert_eq!(hex(&sha256(input)), *s256);
    }
    let million = vec![b'a'; 1_000_000];
    assert_eq!(
        hex(&sha1(&million)),
        "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
    );
    assert_eq!(
        hex(&sha256(&million)),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
    // Padding boundaries (from Python's hashlib over bytes 0, 1, 2, …).
    let data: Vec<u8> = (0..=255u8).cycle().take(120).collect();
    let boundaries = [
        (
            55,
            "8ae2d46729cfe68ff927af5eec9c7d1b66d65ac2",
            "463eb28e72f82e0a96c0a4cc53690c571281131f672aa229e0d45ae59b598b59",
        ),
        (
            56,
            "636e2ec698dac903498e648bd2f3af641d3c88cb",
            "da2ae4d6b36748f2a318f23e7ab1dfdf45acdc9d049bd80e59de82a60895f562",
        ),
        (
            63,
            "6d942da0c4392b123528f2905c713a3ce28364bd",
            "29af2686fd53374a36b0846694cc342177e428d1647515f078784d69cdb9e488",
        ),
        (
            64,
            "c6138d514ffa2135bfce0ed0b8fac65669917ec7",
            "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108",
        ),
        (
            119,
            "41c89d06001bab4ab78736b44efe7ce18ce6ae08",
            "da18797ed7c3a777f0847f429724a2d8cd5138e6ed2895c3fa1a6d39d18f7ec6",
        ),
        (
            120,
            "d3dbd653bd8597b7475321b60a36891278e6a04a",
            "f52b23db1fbb6ded89ef42a23ce0c8922c45f25c50b568a93bf1c075420bbb7c",
        ),
    ];
    for (n, s1, s256) in boundaries {
        assert_eq!(hex(&sha1(&data[..n])), s1, "SHA-1 of {n} bytes");
        assert_eq!(hex(&sha256(&data[..n])), s256, "SHA-256 of {n} bytes");
    }
}

// ---------------------------------------------------------------------------------------------
// Certificates, PEM, CMS, PKCS#12

const LEAF_SHA256: &str = "CC:37:B0:26:F3:3C:12:1C:68:FB:37:66:9B:9C:9D:14:49:CC:E5:80:46:56:A4:1D:93:8C:13:E3:6E:60:3D:89";
const LEAF_SHA1: &str = "13:94:58:F7:13:CF:DC:6C:41:A8:D1:41:39:47:77:55:9E:7B:90:18";

#[test]
fn x509_certificate_in_der() {
    let i = open("x509-leaf.der");
    assert_eq!(i.family, "X.509");
    assert_eq!(fact(&i, "Version"), "v3");
    assert_eq!(fact(&i, "Serial number"), "01:23:45:67:89:ab:cd:ef:01");
    assert_eq!(
        fact(&i, "Subject"),
        "C=LU, L=Esch-sur-Alzette, O=Isolinear Test, CN=test.example"
    );
    assert_eq!(fact(&i, "Subject common name"), "test.example");
    assert_eq!(
        fact(&i, "Issuer"),
        "C=LU, O=Isolinear Test, CN=Isolinear Test CA"
    );
    assert_eq!(fact(&i, "Signature algorithm"), "sha256WithRSAEncryption");
    assert_eq!(fact(&i, "Not before"), "2026-11-01 00:19:09 UTC");
    assert_eq!(fact(&i, "Not after"), "2036-10-29 00:19:09 UTC");
    assert_eq!(
        fact(&i, "Public key algorithm"),
        "EC, curve prime256v1 (P-256)"
    );
    assert_eq!(fact(&i, "Public key size"), "256 bits");
    assert_eq!(fact(&i, "SHA-256 fingerprint"), LEAF_SHA256);
    assert_eq!(fact(&i, "SHA-1 fingerprint"), LEAF_SHA1);
    assert_eq!(fact(&i, "Basic constraints"), "CA: no");
    assert_eq!(fact(&i, "Key usage"), "digital signature, non-repudiation");
    assert_eq!(
        fact(&i, "Extended key usage"),
        "serverAuth, clientAuth, emailProtection"
    );
    assert_eq!(
        fact(&i, "Subject alternative names"),
        "DNS:test.example, DNS:www.test.example, IP:192.0.2.7, email:signer@test.example"
    );
    assert_eq!(
        fact(&i, "CRL distribution points"),
        "URI:http://crl.test.example/ca.crl"
    );
    assert_eq!(
        fact(&i, "Authority information access"),
        "OCSP — URI:http://ocsp.test.example, caIssuers — URI:http://ca.test.example/ca.cer"
    );
    assert!(fact(&i, "Authority key identifier").starts_with("key ID "));
    let ext = table(&i, "Extensions");
    assert_eq!(cell(ext, 1, "Extension"), "keyUsage");
    assert_eq!(cell(ext, 1, "Critical"), "yes");
    assert_eq!(check(&i, Stage::Signature, ""), State::NotDone);
    assert!(i.checks.iter().any(|c| c.stage == Stage::Signature
        && c.detail == "The certificate's signature was not verified"));
    assert_eq!(check(&i, Stage::Schema, ""), State::NotDone);
    assert_eq!(
        check(&i, Stage::Semantic, "signature algorithm inside"),
        State::Passed
    );
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    // Facts point into the file: the subject's range is a DER SEQUENCE.
    let der = std::fs::read(fixture("x509-leaf.der")).unwrap();
    let src = i
        .facts
        .iter()
        .find(|f| f.label == "Subject")
        .unwrap()
        .source
        .clone()
        .unwrap();
    assert_eq!(der[src.start as usize], 0x30);
    assert!(i.outline.len() > 20 && i.outline[0].label == "SEQUENCE");
    assert!(i.notes.iter().any(|n| n.contains("never contacted")));
}

#[test]
fn validity_compared_with_a_clock() {
    use isolinear_formats::inspect::standards::x509::validity_status;
    assert_eq!(validity_status(50, 100, 200).0, State::Failed);
    assert_eq!(validity_status(250, 100, 200).1, "Expired");
    assert_eq!(
        validity_status(150, 100, 200 + 86_400 * 3).1,
        "Valid now (3 days left)"
    );
}

#[test]
fn pem_chain_request_public_key_and_ca() {
    let i = open("x509-chain.pem");
    assert_eq!(i.family, "PEM");
    assert!(i.kind.contains("2 certificates"), "{}", i.kind);
    let blocks = table(&i, "Blocks");
    assert_eq!(blocks.rows.len(), 2);
    assert_eq!(cell(blocks, 0, "Type"), "CERTIFICATE");
    assert!(cell(blocks, 1, "Summary").starts_with("C=LU, O=Isolinear Test, CN=Isolinear Test CA"));
    assert_eq!(
        fact_in(&i, "Certificate 1", "SHA-256 fingerprint"),
        LEAF_SHA256
    );
    assert_eq!(
        fact_in(&i, "Certificate 2", "Basic constraints"),
        "CA: yes, path length 0"
    );
    assert_eq!(
        fact_in(&i, "Certificate 2", "Key usage"),
        "certificate signing, CRL signing"
    );
    assert_eq!(
        fact_in(&i, "Certificate 2", "Public key size"),
        "2048 bits (exponent 65537)"
    );
    assert_eq!(
        fact_in(&i, "Certificate 2", "Self-issued"),
        "Yes (subject and issuer are the same)"
    );
    assert_eq!(table(&i, "Certificates").rows.len(), 2);
    // Sources are the PEM blocks in the file, not offsets in the decoded DER.
    let text = std::fs::read_to_string(fixture("x509-chain.pem")).unwrap();
    let src = blocks.rows[1].source.clone().unwrap();
    assert!(text[src.start as usize..src.end as usize].starts_with("-----BEGIN CERTIFICATE-----"));
    let f = i
        .facts
        .iter()
        .find(|f| f.group == "Certificate 2" && f.label == "Subject")
        .unwrap();
    assert_eq!(f.source, Some(src));

    let i = open("x509-ca.crt");
    assert_eq!(i.kind, "Certificate (PEM)");
    assert_eq!(fact_in(&i, "Certificate", "Serial number"), "4097");

    let i = open("csr.pem");
    assert_eq!(i.kind, "Certificate signing request (PEM)");
    assert_eq!(
        fact(&i, "Subject"),
        "C=LU, L=Esch-sur-Alzette, O=Isolinear Test, CN=test.example"
    );
    assert_eq!(
        fact(&i, "Public key algorithm"),
        "EC, curve prime256v1 (P-256)"
    );
    assert_eq!(fact(&i, "Requested subjectAltName"), "DNS:test.example");
    assert_eq!(check(&i, Stage::Signature, "request"), State::NotDone);

    let i = open("ec-public.pem");
    assert_eq!(i.kind, "Public key (PEM)");
    assert_eq!(fact(&i, "Algorithm"), "EC, curve prime256v1 (P-256)");
    assert_eq!(fact(&i, "Size"), "256 bits");
}

#[test]
fn private_keys_show_only_their_type() {
    let i = open("throwaway-ec-key.pem");
    assert_eq!(i.kind, "Private key (PEM)");
    assert_eq!(
        cell(table(&i, "Blocks"), 0, "Summary"),
        "PKCS#8 private key (ecPublicKey) — private key, not shown"
    );
    assert!(i.notes.iter().any(|n| n.contains("never")));
    let i = open("throwaway-ec-sec1.key");
    assert_eq!(cell(table(&i, "Blocks"), 0, "Type"), "EC PRIVATE KEY");
    assert!(cell(table(&i, "Blocks"), 0, "Summary").contains("EC private key (SEC 1)"));
    // An encrypted legacy key: the cipher is shown, nothing is decoded.
    let enc = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-256-CBC,00112233445566778899AABBCCDDEEFF\n\nAAAAAAAAAAAAAAAA\n-----END RSA PRIVATE KEY-----\n";
    let i = built("legacy.key", enc.as_bytes());
    assert_eq!(
        cell(table(&i, "Blocks"), 0, "Protection"),
        "encrypted (AES-256-CBC)"
    );
    assert_eq!(
        cell(table(&i, "Blocks"), 0, "Summary"),
        "Private key — private key, not shown"
    );
}

/// No part of a private key's content appears anywhere in what is shown.
#[test]
fn private_key_material_never_appears() {
    let pkcs8_text = std::fs::read_to_string(fixture("throwaway-ec-key.pem")).unwrap();
    let sec1_text = std::fs::read_to_string(fixture("throwaway-ec-sec1.key")).unwrap();
    let pkcs8 = pem_body(&pkcs8_text);
    let sec1 = pem_body(&sec1_text);
    // The secret scalar: the first OCTET STRING of 32 bytes in the SEC 1 structure.
    let at = sec1.windows(2).position(|w| w == [0x04, 0x20]).unwrap() + 2;
    let secret = &sec1[at..at + 32];
    assert!(pkcs8.windows(32).any(|w| w == secret));
    let mut needles: Vec<String> = vec![];
    for w in secret.windows(6) {
        needles.push(hex(w));
        needles.push(
            w.iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(":"),
        );
        needles.push(
            w.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":"),
        );
    }
    for text in [&pkcs8_text, &sec1_text] {
        needles.extend(
            text.lines()
                .filter(|l| !l.starts_with("-----") && l.len() > 8)
                .map(|l| l[..12.min(l.len())].to_owned()),
        );
    }
    let dir = std::env::temp_dir().join(format!("isolinear-keys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("key.der"), &pkcs8).unwrap();
    std::fs::write(dir.join("sec1.der"), &sec1).unwrap();
    let inspections = [
        open("throwaway-ec-key.pem"),
        open("throwaway-ec-sec1.key"),
        open("pkcs12-plain.p12"),
        open("pkcs12-store.p12"),
        inspect(&dir.join("key.der")).unwrap().unwrap(),
        inspect(&dir.join("sec1.der")).unwrap().unwrap(),
    ];
    for i in &inspections {
        for text in all_text(i) {
            for n in &needles {
                assert!(
                    !text.contains(n.as_str()),
                    "{n:?} found in {text:?} ({})",
                    i.kind
                );
            }
        }
    }
    // The raw DER keys are recognized as private keys, not outlined.
    assert_eq!(inspections[4].kind, "Private key (DER)");
    assert!(inspections[4].outline.is_empty());
    assert_eq!(
        fact(&inspections[4], "Kind"),
        "PKCS#8 private key (ecPublicKey)"
    );
    assert_eq!(fact(&inspections[5], "Kind"), "EC private key (SEC 1)");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cms_cades_signature_with_content() {
    let i = open("cms-signed.p7m");
    assert_eq!(i.family, "CMS / PKCS#7");
    assert_eq!(i.kind, "Signed data (content enveloped in the signature)");
    assert_eq!(fact(&i, "Content type"), "signedData");
    assert_eq!(fact(&i, "Digest algorithms"), "sha256");
    assert_eq!(fact(&i, "Encapsulated content type"), "data");
    assert!(fact(&i, "Content").starts_with("present in the file (26 bytes)"));
    assert_eq!(fact_in(&i, "Signed data", "Certificates"), "2");
    assert_eq!(fact_in(&i, "Signed data", "Signers"), "1");
    // Certificates are a DER SET, so their order is the encoder's.
    assert!(
        fact_in(&i, "Signer 1", "Signer certificate")
            .ends_with(": C=LU, L=Esch-sur-Alzette, O=Isolinear Test, CN=test.example")
    );
    assert!(fact_in(&i, "Signer 1", "Identified by").contains("serial 01:23:45:67:89:ab:cd:ef:01"));
    assert_eq!(
        fact_in(&i, "Signer 1", "Signature algorithm"),
        "ecdsa-with-SHA256"
    );
    assert_eq!(
        fact_in(&i, "Signer 1", "Message digest"),
        hex(&sha256(b"Isolinear signed content.\n"))
    );
    assert!(fact_in(&i, "Signer 1", "Signing certificate v2 (CAdES)").starts_with("sha256 "));
    assert_eq!(
        check(
            &i,
            Stage::Semantic,
            "message digest matches the encapsulated content"
        ),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "signing-certificate hash matches"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Semantic, "signed content type matches"),
        State::Passed
    );
    assert_eq!(
        check(&i, Stage::Signature, "signature was not verified"),
        State::NotDone
    );
    let signers = table(&i, "Signers");
    assert_eq!(cell(signers, 0, "Digest"), "sha256");
    assert!(!cell(signers, 0, "Signing time").is_empty());
    assert_eq!(table(&i, "Certificates").rows.len(), 2);

    // The same signature over altered content: the digest no longer matches.
    let mut bytes = std::fs::read(fixture("cms-signed.p7m")).unwrap();
    let at = bytes.windows(9).position(|w| w == b"Isolinear").unwrap();
    bytes[at] = b'X';
    let i = built("altered.p7m", &bytes);
    assert_eq!(
        check(&i, Stage::Semantic, "message digest does not match"),
        State::Failed
    );
}

#[test]
fn cms_detached_and_certificates_only() {
    let i = open("cms-detached.p7s");
    assert_eq!(i.kind, "Signature (detached content)");
    assert_eq!(fact(&i, "Content"), "detached (not in this file)");
    assert_eq!(
        check(&i, Stage::Semantic, "content is detached"),
        State::NotDone
    );
    let i = open("cms-certs.p7b");
    assert_eq!(i.kind, "Certificates only (degenerate signed data)");
    let certs = table(&i, "Certificates");
    assert_eq!(certs.rows.len(), 2);
    assert!(certs.rows.iter().any(|r| r.cells[7] == LEAF_SHA256));
}

#[test]
fn pkcs12_stores() {
    let i = open("pkcs12-store.p12");
    assert_eq!(i.family, "PKCS#12");
    assert_eq!(fact(&i, "Version"), "3");
    assert_eq!(fact(&i, "MAC algorithm"), "sha256");
    assert_eq!(fact(&i, "MAC iterations"), "2048");
    assert_eq!(fact(&i, "Encrypted safes (need the password)"), "1");
    assert_eq!(fact(&i, "Private keys"), "1");
    let bags = table(&i, "Bags");
    let enc = bags
        .rows
        .iter()
        .find(|r| r.cells[1] == "Encrypted safe")
        .unwrap();
    assert!(enc.cells[2].starts_with("encrypted — needs the password (PBES2 (PBKDF2, hmacWithSHA256, 2048 iterations, aes256-CBC))"), "{}", enc.cells[2]);
    let key = bags
        .rows
        .iter()
        .find(|r| r.cells[1] == "Private key")
        .unwrap();
    assert!(
        key.cells[2].starts_with("encrypted (PBES2"),
        "{}",
        key.cells[2]
    );
    assert!(key.cells[2].ends_with("not shown"));
    assert_eq!(key.cells[3], "Test leaf");
    assert_eq!(
        check(&i, Stage::Parsed, "password entry is not implemented"),
        State::Partial
    );
    assert_eq!(check(&i, Stage::Signature, "MAC"), State::NotDone);
    assert!(
        i.notes
            .iter()
            .any(|n| n.contains("Password entry is not implemented yet"))
    );

    let i = open("pkcs12-plain.p12");
    assert_eq!(fact(&i, "Certificates (readable)"), "1");
    assert_eq!(
        fact_in(&i, "Certificate 1", "SHA-256 fingerprint"),
        LEAF_SHA256
    );
    let bags = table(&i, "Bags");
    let key = bags
        .rows
        .iter()
        .find(|r| r.cells[1] == "Private key")
        .unwrap();
    assert_eq!(
        key.cells[2],
        "PKCS#8 private key (ecPublicKey), not encrypted — not shown"
    );
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

#[test]
fn generic_der_outline() {
    // SEQUENCE { OID 1.2.840.113549.1.1.11, UTF8String "héllo", [0] { INTEGER 5 } } named .der.
    let mut b = vec![0x30, 0];
    b.extend([
        0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b,
    ]);
    b.extend([0x0c, 0x06]);
    b.extend("héllo".as_bytes());
    b.extend([0xa0, 0x03, 0x02, 0x01, 0x05]);
    b[1] = (b.len() - 2) as u8;
    let i = built("thing.der", &b);
    assert_eq!(i.family, "ASN.1");
    let o: Vec<(u16, &str, &str)> = i
        .outline
        .iter()
        .map(|n| (n.depth, n.label.as_str(), n.value.as_str()))
        .collect();
    assert_eq!(
        o,
        [
            (0, "SEQUENCE", ""),
            (
                1,
                "OBJECT IDENTIFIER",
                "sha256WithRSAEncryption (1.2.840.113549.1.1.11)"
            ),
            (1, "UTF8String", "héllo"),
            (1, "[0]", ""),
            (2, "INTEGER", "5"),
        ]
    );
    assert_eq!(i.outline[2].source, Some(13..21));
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    // Indefinite lengths (BER) are refused with a reason.
    let ber = [0x30, 0x80, 0x02, 0x01, 0x01, 0x00, 0x00];
    assert!(run("ber.der", &ber).is_none_or(|r| r.is_err()));
    // Deep nesting is bounded.
    let mut deep: Vec<u8> = vec![];
    for _ in 0..120 {
        let len = (deep.len() as u16).to_be_bytes();
        deep = [vec![0x30, 0x82, len[0], len[1]], deep].concat();
    }
    let i = built("deep.der", &deep);
    assert!(i.outline.len() <= 41, "{}", i.outline.len());
}

#[test]
fn jwt_decoded_not_verified() {
    let i = open("token.jwt");
    assert_eq!(i.family, "JOSE");
    assert_eq!(fact(&i, "Algorithm"), "HS256");
    assert_eq!(fact(&i, "Type"), "JWT");
    assert_eq!(fact(&i, "Key ID"), "test-key-1");
    assert_eq!(fact(&i, "Issuer"), "https://issuer.test.example");
    assert_eq!(fact(&i, "Subject"), "user-42");
    assert_eq!(fact(&i, "Audience"), "api, web");
    assert_eq!(fact(&i, "Issued at"), "2026-01-01 00:00 UTC");
    assert_eq!(fact(&i, "Expires"), "2100-01-01 00:00 UTC");
    assert_eq!(fact(&i, "Size"), "32 bytes");
    let claims = table(&i, "Claims");
    assert_eq!(cell(claims, 5, "Name"), "exp");
    assert_eq!(
        cell(claims, 5, "Value"),
        "2100-01-01 00:00 UTC (4102444800)"
    );
    assert_eq!(cell(claims, 5, "Meaning"), "Expires");
    assert!(i.checks.iter().any(|c| c.stage == Stage::Signature
        && c.state == State::NotDone
        && c.detail == "Decoded only; the signature was not verified"));
    assert_eq!(check(&i, Stage::Semantic, "Expiry"), State::Passed);
    // JWE: five parts, header read, nothing decrypted.
    let h = "eyJhbGciOiJSU0EtT0FFUCIsImVuYyI6IkEyNTZHQ00ifQ"; // {"alg":"RSA-OAEP","enc":"A256GCM"}
    let jwe = format!("{h}.AAAA.AAAAAAAAAAAAAAAA.AAAAAAAA.AAAAAAAAAAAAAAAAAAAAAA");
    let i = built("t.jwt", jwe.as_bytes());
    assert!(i.kind.starts_with("JSON Web Encryption"));
    assert_eq!(fact(&i, "Content encryption"), "A256GCM");
    assert_eq!(check(&i, Stage::Signature, "Not decrypted"), State::NotDone);
    // Three dotted words that are not a token are not claimed.
    assert!(run("notes.txt", b"hello.world.again").is_none());
}

// ---------------------------------------------------------------------------------------------
// WebAssembly

fn wasm_section(id: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![id, content.len() as u8];
    assert!(content.len() < 128);
    v.extend_from_slice(content);
    v
}
fn wname(s: &str) -> Vec<u8> {
    let mut v = vec![s.len() as u8];
    v.extend_from_slice(s.as_bytes());
    v
}

fn wasm_module() -> Vec<u8> {
    let mut m = b"\0asm\x01\0\0\0".to_vec();
    m.extend(wasm_section(
        1,
        &[2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 0],
    ));
    let mut imp = vec![1];
    imp.extend(wname("env"));
    imp.extend(wname("log"));
    imp.extend([0, 1]);
    m.extend(wasm_section(2, &imp));
    m.extend(wasm_section(3, &[2, 0, 1]));
    m.extend(wasm_section(5, &[1, 1, 1, 16]));
    let mut exp = vec![2];
    exp.extend(wname("add"));
    exp.extend([0, 1]);
    exp.extend(wname("memory"));
    exp.extend([2, 0]);
    m.extend(wasm_section(7, &exp));
    m.extend(wasm_section(
        10,
        &[2, 7, 0, 0x20, 0, 0x20, 1, 0x6a, 0x0b, 4, 0, 0x10, 0, 0x0b],
    ));
    m.extend(wasm_section(11, &[1, 0, 0x41, 0, 0x0b, 2, b'h', b'i']));
    let mut names = vec![2];
    names.extend([1]);
    names.extend(wname("add"));
    names.extend([2]);
    names.extend(wname("run"));
    let mut name = wname("name");
    name.push(1);
    name.push(names.len() as u8);
    name.extend(names);
    m.extend(wasm_section(0, &name));
    let mut prod = wname("producers");
    prod.push(1);
    prod.extend(wname("processed-by"));
    prod.push(1);
    prod.extend(wname("hand"));
    prod.extend(wname("1.0"));
    m.extend(wasm_section(0, &prod));
    m
}

#[test]
fn webassembly_module() {
    let i = built("demo.wasm", &wasm_module());
    assert_eq!(i.family, "WebAssembly");
    assert_eq!(fact(&i, "Binary version"), "1");
    assert_eq!(fact(&i, "Sections"), "9");
    assert_eq!(fact(&i, "Functions defined"), "2");
    assert_eq!(
        fact(&i, "Imports"),
        "1 (1 functions, 0 tables, 0 memories, 0 globals)"
    );
    assert_eq!(fact(&i, "Data segments"), "1");
    assert_eq!(fact(&i, "processed-by"), "hand 1.0");
    let types = table(&i, "Types");
    assert_eq!(cell(types, 0, "Signature"), "(i32, i32) -> (i32)");
    let imports = table(&i, "Imports");
    assert_eq!(
        imports.rows[0].cells,
        ["env", "log", "function", "() -> ()"]
    );
    let exports = table(&i, "Exports");
    assert_eq!(exports.rows[1].cells, ["memory", "memory", "0"]);
    let funcs = table(&i, "Functions");
    assert_eq!(
        funcs.rows[0].cells,
        ["1", "add", "(i32, i32) -> (i32)", "7 bytes", "add"]
    );
    assert_eq!(funcs.rows[1].cells[1], "run");
    let mems = table(&i, "Memories and tables");
    assert_eq!(cell(mems, 0, "Limits"), "min 1, max 16 (pages of 64 KiB)");
    assert_eq!(check(&i, Stage::Semantic, "agree"), State::Passed);
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    // A section claiming more than the file holds.
    let mut cut = wasm_module();
    cut.truncate(30);
    let i = built("cut.wasm", &cut);
    assert_eq!(check(&i, Stage::Parsed, "Damaged"), State::Partial);
}

// ---------------------------------------------------------------------------------------------
// Java class files

fn class_file() -> Vec<u8> {
    let mut c = vec![0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 61];
    let utf8 = |s: &str| {
        let mut v = vec![1, 0, s.len() as u8];
        v.extend_from_slice(s.as_bytes());
        v
    };
    let mut pool: Vec<Vec<u8>> = vec![
        utf8("demo/Hello"),
        vec![7, 0, 1],
        utf8("java/lang/Object"),
        vec![7, 0, 3],
        utf8("count"),
        utf8("I"),
        utf8("greet"),
        utf8("(Ljava/lang/String;[J)V"),
        utf8("Code"),
        utf8("SourceFile"),
        utf8("Hello.java"),
        vec![5, 0, 0, 0x01, 0x1f, 0x71, 0xfb, 0x04, 0xcb], // Long 1234567890123 (two slots: 12, 13)
        utf8("java/lang/Runnable"),
        vec![7, 0, 14],
        utf8("RuntimeVisibleAnnotations"),
        utf8("Ljava/lang/Deprecated;"),
    ];
    c.extend([0, 18]);
    for e in pool.drain(..) {
        c.extend(e);
    }
    c.extend([0, 0x21, 0, 2, 0, 4]); // public super, this #2, super #4
    c.extend([0, 1, 0, 15]); // implements Runnable
    c.extend([0, 1, 0, 0x0a, 0, 5, 0, 6, 0, 0]); // private static int count
    c.extend([0, 1, 0, 1, 0, 7, 0, 8, 0, 1]); // public void greet(String, long[])
    c.extend([0, 9, 0, 0, 0, 13, 0, 0, 0, 3, 0, 0, 0, 1, 0xb1, 0, 0, 0, 0]);
    c.extend([0, 2]);
    c.extend([0, 10, 0, 0, 0, 2, 0, 11]);
    c.extend([0, 16, 0, 0, 0, 6, 0, 1, 0, 17, 0, 0]);
    c
}

#[test]
fn java_class_file() {
    let i = built("Hello.class", &class_file());
    assert_eq!(i.family, "Java");
    assert_eq!(fact(&i, "Java version"), "Java 17");
    assert_eq!(fact(&i, "Name"), "demo.Hello");
    assert_eq!(fact(&i, "Super class"), "java.lang.Object");
    assert_eq!(fact(&i, "Interfaces"), "java.lang.Runnable");
    assert_eq!(fact(&i, "Modifiers"), "public, super");
    assert_eq!(fact(&i, "Source file"), "Hello.java");
    assert_eq!(fact(&i, "Annotations"), "@java.lang.Deprecated");
    assert_eq!(fact(&i, "Constant pool entries"), "17");
    let fields = table(&i, "Fields");
    assert_eq!(
        fields.rows[0].cells[..3],
        ["count", "int", "private, static"]
    );
    let methods = table(&i, "Methods");
    assert_eq!(
        methods.rows[0].cells[..4],
        [
            "greet",
            "void (java.lang.String, long[])",
            "public",
            "1 bytes"
        ]
    );
    let cp = table(&i, "Constant pool");
    assert!(
        cp.rows
            .iter()
            .any(|r| r.cells == ["#12", "Long", "1234567890123"])
    );
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    // A universal Mach-O binary shares the magic: not taken as a class.
    let fat = [
        0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2, 0, 0, 0, 7, 0, 0, 0, 3, 0, 0, 0x10, 0, 0, 0, 0x10, 0,
    ];
    assert!(run("prog", &fat).is_none_or(|r| r.map(|i| i.family != "Java").unwrap_or(true)));
    assert!(!isolinear_formats::inspect::standards::class::is_class(
        &fat
    ));
}

// ---------------------------------------------------------------------------------------------
// ELF

#[test]
fn elf_executable_and_library() {
    let i = open("elf-demo");
    assert_eq!(i.family, "ELF");
    assert_eq!(fact(&i, "Class"), "64-bit");
    assert_eq!(fact(&i, "Byte order"), "little-endian");
    assert_eq!(fact(&i, "Machine"), "x86-64 (0x3e)");
    assert_eq!(fact(&i, "Entry point"), "0x1090");
    assert_eq!(fact(&i, "Interpreter"), "/lib64/ld-linux-x86-64.so.2");
    assert_eq!(fact(&i, "Needed libraries"), "libc.so.6");
    assert_eq!(fact(&i, "RUNPATH"), "/opt/isolinear/lib");
    assert_eq!(
        fact(&i, "Build ID"),
        "d00ce7e42ff9f5eba8b3c7efafe66646736acbf6"
    );
    assert!(fact(&i, "ABI tag").starts_with("Linux "));
    let secs = table(&i, "Sections");
    assert!(
        secs.rows.iter().any(|r| r.cells[1] == ".text"
            && r.cells[2] == "PROGBITS"
            && r.cells[3] == "alloc, exec")
    );
    let syms = table(&i, "Symbols");
    assert!(syms.rows.iter().any(|r| r.cells[1] == "isolinear_answer"
        && r.cells[4] == "FUNC"
        && r.cells[5] == "GLOBAL"
        && r.cells[6] == ".text"));
    assert!(syms.rows.iter().any(|r| r.cells[0] == ".dynsym"
        && r.cells[1] == "__libc_start_main"
        && r.cells[6] == "undefined"));
    let ph = table(&i, "Program headers");
    assert!(ph.rows.iter().any(|r| r.cells[0] == "INTERP"));
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);

    let i = open("elf-libdemo.so");
    assert_eq!(fact(&i, "SONAME"), "libdemo.so.1");
    assert_eq!(
        fact(&i, "Build ID"),
        "4e96293b4fd58bc1436115eb4d67cab605cedaaf"
    );
    assert!(
        table(&i, "Symbols")
            .rows
            .iter()
            .any(|r| r.cells[1] == "isolinear_lib")
    );
    assert!(
        table(&i, "Dynamic section")
            .rows
            .iter()
            .any(|r| r.cells == ["SONAME", "libdemo.so.1"])
    );
}

// ---------------------------------------------------------------------------------------------
// PE

fn put(b: &mut Vec<u8>, at: usize, bytes: &[u8]) {
    if b.len() < at + bytes.len() {
        b.resize(at + bytes.len(), 0);
    }
    b[at..at + bytes.len()].copy_from_slice(bytes);
}
fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}
/// A VS_VERSIONINFO-style block.
fn vblock(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0, 0];
    let vlen = if text { value.len() / 2 } else { value.len() } as u16;
    b.extend(vlen.to_le_bytes());
    b.extend(u16::from(text).to_le_bytes());
    b.extend(utf16z(key));
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
    b.extend(value);
    for c in children {
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        b.extend(c);
    }
    let len = b.len() as u16;
    b[..2].copy_from_slice(&len.to_le_bytes());
    b
}

/// A PE32+ DLL with imports, exports, a version resource, a CLR header and a signature.
fn pe_file() -> Vec<u8> {
    let le16 = |v: u16| v.to_le_bytes();
    let le32 = |v: u32| v.to_le_bytes();
    let mut f = vec![0u8; 0x200];
    put(&mut f, 0, b"MZ");
    put(&mut f, 0x3c, &le32(0x40));
    put(&mut f, 0x40, b"PE\0\0");
    put(&mut f, 0x44, &le16(0x8664));
    put(&mut f, 0x46, &le16(1));
    put(&mut f, 0x48, &le32(0x6543_2100));
    put(&mut f, 0x54, &le16(240));
    put(&mut f, 0x56, &le16(0x2022));
    let o = 0x58;
    put(&mut f, o, &le16(0x20b));
    put(&mut f, o + 2, &[14, 0]);
    put(&mut f, o + 16, &le32(0x1000));
    put(&mut f, o + 24, &0x1_8000_0000u64.to_le_bytes());
    put(&mut f, o + 32, &le32(0x1000));
    put(&mut f, o + 36, &le32(0x200));
    put(&mut f, o + 40, &le16(6));
    put(&mut f, o + 68, &le16(3));
    put(&mut f, o + 70, &le16(0x0160));
    put(&mut f, o + 108, &le32(16));
    let dirs = o + 112;
    put(&mut f, dirs, &[le32(0x1100), le32(0x60)].concat()); // export
    put(&mut f, dirs + 8, &[le32(0x1000), le32(0x28)].concat()); // import
    put(&mut f, dirs + 16, &[le32(0x1200), le32(0x60)].concat()); // resource
    put(&mut f, dirs + 14 * 8, &[le32(0x1380), le32(72)].concat()); // CLR
    let st = o + 240;
    put(&mut f, st, b".text\0\0\0");
    put(
        &mut f,
        st + 8,
        &[le32(0x1000), le32(0x1000), le32(0x400), le32(0x200)].concat(),
    );
    put(&mut f, st + 36, &le32(0x6000_0020));
    let r = |rva: usize| rva - 0x1000 + 0x200;
    // Imports.
    put(
        &mut f,
        r(0x1000),
        &[le32(0x1040), le32(0), le32(0), le32(0x1060), le32(0x1040)].concat(),
    );
    put(&mut f, r(0x1040), &0x1070u64.to_le_bytes());
    put(&mut f, r(0x1048), &0x8000_0000_0000_0005u64.to_le_bytes());
    put(&mut f, r(0x1060), b"KERNEL32.dll\0");
    put(&mut f, r(0x1070), &le16(0x123));
    put(&mut f, r(0x1072), b"ExitProcess\0");
    // Exports.
    let e = r(0x1100);
    put(&mut f, e + 12, &le32(0x1140));
    put(
        &mut f,
        e + 16,
        &[
            le32(1),
            le32(2),
            le32(1),
            le32(0x1150),
            le32(0x1160),
            le32(0x1170),
        ]
        .concat(),
    );
    put(&mut f, r(0x1140), b"demo.dll\0");
    put(&mut f, r(0x1150), &[le32(0x1000), le32(0x1010)].concat());
    put(&mut f, r(0x1160), &le32(0x1180));
    put(&mut f, r(0x1170), &le16(0));
    put(&mut f, r(0x1180), b"DemoFunc\0");
    // Resources: version -> 1 -> language 0x409 -> data.
    let res = r(0x1200);
    put(&mut f, res + 14, &le16(1));
    put(&mut f, res + 16, &[le32(16), le32(0x8000_0018)].concat());
    put(&mut f, res + 0x18 + 14, &le16(1));
    put(
        &mut f,
        res + 0x18 + 16,
        &[le32(1), le32(0x8000_0030)].concat(),
    );
    put(&mut f, res + 0x30 + 14, &le16(1));
    put(&mut f, res + 0x30 + 16, &[le32(0x409), le32(0x48)].concat());
    let mut fixed = vec![0u8; 52];
    fixed[..4].copy_from_slice(&le32(0xfeef_04bd));
    fixed[8..12].copy_from_slice(&le32(0x0001_0002));
    fixed[12..16].copy_from_slice(&le32(0x0003_0004));
    let s = vblock("CompanyName", &utf16z("Isolinear Test"), true, &[]);
    let table = vblock("040904b0", &[], true, &[s]);
    let sfi = vblock("StringFileInfo", &[], true, &[table]);
    let vi = vblock("VS_VERSION_INFO", &fixed, false, &[sfi]);
    assert!(vi.len() < 0x120);
    put(
        &mut f,
        res + 0x48,
        &[le32(0x1260), le32(vi.len() as u32)].concat(),
    );
    put(&mut f, r(0x1260), &vi);
    // CLR header and metadata root.
    put(
        &mut f,
        r(0x1380),
        &[le32(72), [2, 0, 5, 0], le32(0x13d0), le32(0x30), le32(1)].concat(),
    );
    put(
        &mut f,
        r(0x13d0),
        &[le32(0x424a_5342), [1, 0, 1, 0], le32(0), le32(12)].concat(),
    );
    put(&mut f, r(0x13d0) + 16, b"v4.0.30319\0\0");
    f.resize(0x600, 0);
    // Authenticode-style certificate table holding a CMS signature.
    let cms = std::fs::read(fixture("cms-signed.p7m")).unwrap();
    let mut cert = vec![];
    cert.extend(le32(8 + cms.len() as u32));
    cert.extend(le16(0x200));
    cert.extend(le16(2));
    cert.extend(&cms);
    put(
        &mut f,
        dirs + 4 * 8,
        &[le32(0x600), le32(cert.len() as u32)].concat(),
    );
    f.extend(cert);
    f
}

#[test]
fn pe_dll() {
    let i = built("demo.dll", &pe_file());
    assert_eq!(i.family, "PE");
    assert_eq!(i.kind, "Windows library (DLL)");
    assert_eq!(i.version.as_deref(), Some("PE32+"));
    assert_eq!(fact(&i, "Machine"), "x86-64 (0x8664)");
    assert_eq!(fact(&i, "Subsystem"), "Windows console");
    assert_eq!(fact(&i, "Image base"), "0x180000000");
    assert_eq!(fact(&i, "Entry point (RVA)"), "0x1000");
    assert_eq!(
        fact(&i, "Characteristics"),
        "executable, large address aware, DLL"
    );
    assert_eq!(
        fact(&i, "DLL characteristics"),
        "high-entropy ASLR, ASLR (dynamic base), DEP (NX compatible)"
    );
    assert_eq!(fact(&i, "Libraries"), "KERNEL32.dll");
    let imports = table(&i, "Imports");
    assert_eq!(
        imports.rows[0].cells,
        ["KERNEL32.dll", "ExitProcess", "291"]
    );
    assert_eq!(imports.rows[1].cells, ["KERNEL32.dll", "", "ordinal 5"]);
    assert_eq!(fact(&i, "Library name"), "demo.dll");
    let exports = table(&i, "Exports");
    assert_eq!(exports.rows[0].cells, ["1", "DemoFunc", "0x1000"]);
    assert_eq!(exports.rows[1].cells, ["2", "", "0x1010"]);
    assert_eq!(table(&i, "Resources").rows[0].cells, ["version", "1"]);
    assert_eq!(fact(&i, "CompanyName"), "Isolinear Test");
    assert_eq!(fact(&i, "File version (fixed)"), "1.2.3.4");
    assert_eq!(fact(&i, "Runtime version"), "v4.0.30319");
    assert!(fact(&i, "CLR header").starts_with("present"));
    assert_eq!(fact(&i, "Kind"), "Authenticode (PKCS#7 signed data)");
    assert!(fact_in(&i, "Authenticode signer 1", "Signer certificate").contains("test.example"));
    assert_eq!(check(&i, Stage::Signature, "Authenticode"), State::NotDone);
    assert_eq!(table(&i, "Sections").rows[0].cells[0], ".text");
    assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
}

// ---------------------------------------------------------------------------------------------
// Packet captures

fn eth(ty: u16, payload: &[u8], vlan: Option<u16>) -> Vec<u8> {
    let mut v = vec![0x02, 0, 0, 0, 0, 0x02, 0x02, 0, 0, 0, 0, 0x01];
    if let Some(id) = vlan {
        v.extend(0x8100u16.to_be_bytes());
        v.extend(id.to_be_bytes());
    }
    v.extend(ty.to_be_bytes());
    v.extend(payload);
    v
}
fn ip4(proto: u8, src: [u8; 4], dst: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0x45, 0];
    v.extend((20 + payload.len() as u16).to_be_bytes());
    v.extend([0, 1, 0x40, 0, 64, proto, 0, 0]);
    v.extend(src);
    v.extend(dst);
    v.extend(payload);
    v
}
fn ip6(next: u8, src: [u8; 16], dst: [u8; 16], payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0x60, 0, 0, 0];
    v.extend((payload.len() as u16).to_be_bytes());
    v.extend([next, 64]);
    v.extend(src);
    v.extend(dst);
    v.extend(payload);
    v
}
fn udp(sp: u16, dp: u16, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![];
    v.extend(sp.to_be_bytes());
    v.extend(dp.to_be_bytes());
    v.extend((8 + payload.len() as u16).to_be_bytes());
    v.extend([0, 0]);
    v.extend(payload);
    v
}
fn tcp(sp: u16, dp: u16, seq: u32, flags: u8) -> Vec<u8> {
    let mut v = vec![];
    v.extend(sp.to_be_bytes());
    v.extend(dp.to_be_bytes());
    v.extend(seq.to_be_bytes());
    v.extend(0u32.to_be_bytes());
    v.extend([0x50, flags]);
    v.extend(64240u16.to_be_bytes());
    v.extend([0, 0, 0, 0]);
    v
}
fn dns_query(id: u16, response: bool) -> Vec<u8> {
    let mut v = vec![];
    v.extend(id.to_be_bytes());
    v.extend(if response { [0x81, 0x80] } else { [0x01, 0x00] });
    v.extend([0, 1, 0, u8::from(response), 0, 0, 0, 0]);
    v.extend(b"\x07example\x03com\x00");
    v.extend([0, 1, 0, 1]);
    if response {
        v.extend([
            0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0x0e, 0x10, 0, 4, 93, 184, 216, 34,
        ]);
    }
    v
}

fn packets() -> Vec<Vec<u8>> {
    let a = [192, 0, 2, 1];
    let b = [198, 51, 100, 2];
    let mut v6a = [0u8; 16];
    v6a[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
    let mut v6b = v6a;
    v6a[15] = 1;
    v6b[15] = 2;
    let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1, 2, 0, 0, 0, 0, 1];
    arp.extend(a);
    arp.extend([0; 6]);
    arp.extend([192, 0, 2, 254]);
    vec![
        eth(
            0x0800,
            &ip4(17, a, b, &udp(53000, 53, &dns_query(0x1234, false))),
            None,
        ),
        eth(
            0x0800,
            &ip4(6, a, b, &tcp(40000, 443, 1000, 0x02)),
            Some(100),
        ),
        eth(0x0806, &arp, None),
        eth(
            0x86dd,
            &ip6(58, v6a, v6b, &[128, 0, 0, 0, 0, 7, 0, 1]),
            None,
        ),
        eth(0x0800, &ip4(1, a, b, &[8, 0, 0, 0, 0, 9, 0, 2]), None),
    ]
}

fn pcap(le: bool, nanos: bool, link: u32, packets: &[Vec<u8>]) -> Vec<u8> {
    let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
    let mut f = vec![];
    f.extend(w32(if nanos { 0xa1b2_3c4d } else { 0xa1b2_c3d4 }));
    f.extend(w16(2));
    f.extend(w16(4));
    f.extend([0; 8]);
    f.extend(w32(65535));
    f.extend(w32(link));
    for (n, p) in packets.iter().enumerate() {
        f.extend(w32(1_767_225_600 + n as u32));
        f.extend(w32(if nanos { 500_000_000 } else { 500_000 }));
        f.extend(w32(p.len() as u32));
        f.extend(w32(p.len() as u32));
        f.extend(p);
    }
    f
}

#[test]
fn pcap_packets_decoded() {
    for (le, nanos) in [(true, false), (false, true)] {
        let i = built("capture.pcap", &pcap(le, nanos, 1, &packets()));
        assert_eq!(i.family, "Packet capture");
        assert_eq!(fact(&i, "Link type"), "Ethernet");
        assert_eq!(fact(&i, "Packets"), "5");
        assert_eq!(fact(&i, "First packet"), "2026-01-01 00:00:00.500000 UTC");
        assert_eq!(fact(&i, "Time span"), "4.000000 s");
        let t = table(&i, "Packets");
        assert_eq!(
            t.rows[0].cells,
            [
                "1",
                "0.000000",
                "192.0.2.1:53000",
                "198.51.100.2:53",
                "DNS",
                "71",
                "Standard query 0x1234 A example.com"
            ]
        );
        assert_eq!(cell(t, 1, "Protocol"), "TCP");
        assert_eq!(
            cell(t, 1, "Info"),
            "VLAN 100 40000 → 443 [SYN] Seq=1000 Ack=0 Win=64240 Len=0"
        );
        assert_eq!(cell(t, 2, "Info"), "Who has 192.0.2.254? Tell 192.0.2.1");
        assert_eq!(cell(t, 3, "Source"), "2001:db8::1");
        assert_eq!(cell(t, 3, "Info"), "Echo request id=7 seq=1");
        assert_eq!(cell(t, 4, "Info"), "Echo request id=9 seq=2");
        assert!(fact(&i, "Protocols").contains("DNS 1"));
        assert_eq!(t.rows[0].source, Some(24..24 + 16 + 71));
        assert_eq!(check(&i, Stage::Parsed, ""), State::Passed);
    }
    // A cut last packet: what was read is shown, marked partial.
    let mut f = pcap(true, false, 1, &packets());
    f.truncate(f.len() - 3);
    let i = built("cut.pcap", &f);
    assert_eq!(fact(&i, "Packets"), "4");
    assert_eq!(check(&i, Stage::Parsed, "cut"), State::Partial);
}

#[test]
fn pcap_counts_past_the_row_limit() {
    let p = eth(
        0x0800,
        &ip4(17, [10, 0, 0, 1], [10, 0, 0, 2], &udp(1, 2, &[])),
        None,
    );
    let many: Vec<Vec<u8>> = std::iter::repeat_n(p, MAX_ROWS + 7).collect();
    let started = std::time::Instant::now();
    let i = built("big.pcap", &pcap(true, false, 1, &many));
    assert!(started.elapsed().as_secs() < 60);
    let t = table(&i, "Packets");
    assert_eq!(t.rows.len(), MAX_ROWS);
    assert_eq!(t.total, Some((MAX_ROWS + 7) as u64));
}

fn pcapng_be() -> Vec<u8> {
    let be32 = |v: u32| v.to_be_bytes();
    let block = |ty: u32, body: &[u8]| {
        let mut body = body.to_vec();
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
        let len = (body.len() + 12) as u32;
        let mut v = vec![];
        v.extend(be32(ty));
        v.extend(be32(len));
        v.extend(body);
        v.extend(be32(len));
        v
    };
    let mut f = vec![];
    let mut shb = vec![];
    shb.extend(be32(0x1a2b_3c4d));
    shb.extend([0, 1, 0, 0]);
    shb.extend((-1i64).to_be_bytes());
    shb.extend([0, 2, 0, 4]);
    shb.extend(b"test");
    shb.extend([0, 0, 0, 0]);
    f.extend(block(0x0a0d_0d0a, &shb));
    let mut idb = vec![0, 113, 0, 0];
    idb.extend(be32(262_144));
    idb.extend([0, 2, 0, 4]);
    idb.extend(b"eth0");
    idb.extend([0, 9, 0, 1, 9, 0, 0, 0]);
    idb.extend([0, 0, 0, 0]);
    f.extend(block(1, &idb));
    // Linux cooked capture: packet type, ARPHRD, address length, address, protocol.
    let mut sll = vec![0, 0, 0, 1, 0, 6, 2, 0, 0, 0, 0, 1, 0, 0, 0x08, 0x00];
    sll.extend(ip4(
        17,
        [9, 9, 9, 9],
        [192, 0, 2, 1],
        &udp(53, 53000, &dns_query(0x1234, true)),
    ));
    let ts: u64 = 1_767_225_600_123_456_789;
    let mut epb = vec![];
    epb.extend(be32(0));
    epb.extend(be32((ts >> 32) as u32));
    epb.extend(be32(ts as u32));
    epb.extend(be32(sll.len() as u32));
    epb.extend(be32(sll.len() as u32));
    epb.extend(&sll);
    f.extend(block(6, &epb));
    f.extend(block(0x0000_000a, b"secrets"));
    f
}

#[test]
fn pcapng_big_endian_with_cooked_capture() {
    let i = built("capture.pcapng", &pcapng_be());
    assert!(fact(&i, "Format").contains("big-endian"));
    assert_eq!(fact(&i, "Hardware"), "test");
    assert_eq!(fact(&i, "Link type"), "Linux cooked capture (SLL)");
    assert_eq!(fact(&i, "Packets"), "1");
    assert_eq!(fact(&i, "First packet"), "2026-01-01 00:00:00.123456 UTC");
    let ifs = table(&i, "Interfaces");
    assert_eq!(cell(ifs, 0, "Name"), "eth0");
    assert_eq!(cell(ifs, 0, "Time resolution"), "1/1,000,000,000 s");
    let t = table(&i, "Packets");
    assert_eq!(
        cell(t, 0, "Info"),
        "Standard query response 0x1234 A example.com A 93.184.216.34"
    );
    assert_eq!(
        fact(&i, "Decryption secrets blocks"),
        "1 (contents not shown)"
    );
    assert!(
        !all_text(&i)
            .iter()
            .any(|s| s.contains("secrets") && s.len() == 7)
    );
}

// ---------------------------------------------------------------------------------------------
// Damage: truncation and random mutation of every input, directly through the handlers.

#[test]
fn damaged_inputs_never_panic_and_stay_quick() {
    let mut inputs: Vec<(String, Vec<u8>)> = [
        "x509-leaf.der",
        "x509-chain.pem",
        "x509-ca.crt",
        "csr.pem",
        "ec-public.pem",
        "throwaway-ec-key.pem",
        "cms-signed.p7m",
        "cms-detached.p7s",
        "cms-certs.p7b",
        "pkcs12-store.p12",
        "pkcs12-plain.p12",
        "token.jwt",
        "elf-demo",
        "elf-libdemo.so",
    ]
    .iter()
    .map(|n| (n.to_string(), std::fs::read(fixture(n)).unwrap()))
    .collect();
    inputs.push(("demo.wasm".into(), wasm_module()));
    inputs.push(("Hello.class".into(), class_file()));
    inputs.push(("demo.dll".into(), pe_file()));
    inputs.push(("capture.pcap".into(), pcap(true, false, 1, &packets())));
    inputs.push(("capture.pcapng".into(), pcapng_be()));
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for (name, bytes) in &inputs {
        // Every handler that claims the intact file must also handle it cut anywhere.
        assert!(run(name, bytes).is_some(), "{name} not recognized");
        let step = (bytes.len() / 64).max(1);
        for cut in (0..bytes.len()).step_by(step) {
            let started = std::time::Instant::now();
            let _ = run(name, &bytes[..cut]);
            assert!(started.elapsed().as_secs() < 2, "{name} cut at {cut}");
        }
        for _ in 0..150 {
            let mut b = bytes.clone();
            for _ in 0..1 + xorshift(&mut seed) % 8 {
                let at = (xorshift(&mut seed) % b.len() as u64) as usize;
                b[at] = (xorshift(&mut seed) >> 32) as u8;
            }
            let started = std::time::Instant::now();
            let _ = run(name, &b);
            assert!(started.elapsed().as_secs() < 2, "{name} mutated");
        }
    }
    // Hostile lengths: huge claimed sizes and counts.
    let hostile: Vec<(&str, Vec<u8>)> = vec![
        (
            "a.der",
            vec![0x30, 0x84, 0xff, 0xff, 0xff, 0xff, 0x30, 0x00],
        ),
        (
            "a.wasm",
            b"\0asm\x01\0\0\0\x01\xff\xff\xff\xff\x0f\x01".to_vec(),
        ),
        (
            "a.class",
            vec![
                0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52, 0xff, 0xff, 1, 0xff, 0xff,
            ],
        ),
        (
            "a.pcap",
            pcap(true, false, 1, &[])
                .into_iter()
                .chain([0xff; 16])
                .collect(),
        ),
        ("a.exe", {
            let mut f = pe_file();
            f[0x46] = 0xff;
            f[0x47] = 0xff;
            f
        }),
        ("a", {
            let mut f = std::fs::read(fixture("elf-demo")).unwrap();
            f[0x3c] = 0xff;
            f[0x3d] = 0xff;
            f
        }),
    ];
    for (name, b) in hostile {
        let started = std::time::Instant::now();
        let _ = run(name, &b);
        assert!(started.elapsed().as_secs() < 2, "{name}");
    }
}

/// The files built above are also kept in the fixtures folder, so they can be opened by hand
/// and in the app. `ISOLINEAR_WRITE_FIXTURES=1` rewrites them after a builder changes.
#[test]
fn built_files_are_kept_as_fixtures() {
    let files = [
        ("demo.wasm", wasm_module()),
        ("Hello.class", class_file()),
        ("demo.dll", pe_file()),
        ("capture.pcap", pcap(true, false, 1, &packets())),
        ("capture.pcapng", pcapng_be()),
    ];
    for (name, bytes) in files {
        let path = fixture(name);
        if std::env::var_os("ISOLINEAR_WRITE_FIXTURES").is_some() {
            std::fs::write(&path, &bytes).unwrap();
        }
        let kept = std::fs::read(&path).unwrap_or_default();
        assert!(
            kept == bytes,
            "{name} differs from its builder; rerun with ISOLINEAR_WRITE_FIXTURES=1"
        );
        assert_eq!(open(name).family, built(name, &bytes).family, "{name}");
    }
}
