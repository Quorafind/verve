//! A certificate for `localhost`, made when the tests run. Verve's repository is public, so it
//! keeps no private key: each test process signs its own self-signed certificate (CN and SANs
//! `localhost`, `127.0.0.1`, `::1`) and trusts it as the only CA where a test asks for one.

use std::sync::OnceLock;

/// The PEM certificate and its PKCS #8 private key.
pub struct Identity {
    pub cert: String,
    pub key: String,
}

/// This process's localhost identity, made on first use.
pub fn localhost() -> &'static Identity {
    static IDENTITY: OnceLock<Identity> = OnceLock::new();
    IDENTITY.get_or_init(|| {
        let mut params = rcgen::CertificateParams::new(vec![
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
            "::1".to_owned(),
        ])
        .expect("localhost names");
        let mut name = rcgen::DistinguishedName::new();
        name.push(rcgen::DnType::CommonName, "localhost");
        name.push(rcgen::DnType::OrganizationName, "verve test fixture");
        params.distinguished_name = name;
        let key = rcgen::KeyPair::generate().expect("a key pair");
        let cert = params.self_signed(&key).expect("a self-signed certificate");
        Identity {
            cert: cert.pem(),
            key: key.serialize_pem(),
        }
    })
}

/// A script prelude that defines `TLS_CERT` and `TLS_KEY` as Buffers of the identity's PEM.
pub fn prelude() -> String {
    let identity = localhost();
    format!(
        "globalThis.TLS_CERT = Buffer.from({}); globalThis.TLS_KEY = Buffer.from({});",
        serde_json::Value::from(identity.cert.as_str()),
        serde_json::Value::from(identity.key.as_str()),
    )
}
