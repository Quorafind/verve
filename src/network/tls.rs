//! rustls TLS runs in the existing reactor, including system trust loading and handshakes.
//! JavaScript receives identity hooks and certificates as queued events, never on I/O threads.
use super::{
    reactor::{Command, Id},
    tcp,
};
use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error_value, function, get, hidden, invoke,
    js_text, list_of, object, text_of,
};
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use std::{
    io::{self, Cursor},
    sync::Arc,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct Options {
    pub host: String,
    pub ca: Vec<Vec<u8>>,
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
    pub alpn: Vec<Vec<u8>>,
    pub reject: bool,
    pub min: String,
    pub max: String,
    pub ciphers: String,
    pub contexts: Vec<(String, Options)>,
    pub request_cert: bool,
}
pub(crate) fn options(value: &JsValue, host: &str, cx: &mut Context) -> JsResult<Options> {
    let secure = get(value, "secureContext", cx)?;
    let fallback = get(&secure, "__tlsOptions", cx)?;
    let field = |name: &str, cx: &mut Context| -> JsResult<JsValue> {
        let v = get(value, name, cx)?;
        if v.is_undefined() {
            get(&fallback, name, cx)
        } else {
            Ok(v)
        }
    };
    let bytes = |v: &JsValue, cx: &mut Context| -> JsResult<Vec<u8>> {
        Ok(crate::encoding::bytes_of(v, cx)?.unwrap_or_else(|| {
            if v.is_undefined() {
                Vec::new()
            } else {
                crate::print(v).into_bytes()
            }
        }))
    };
    let mut ca = Vec::new();
    let roots = field("ca", cx)?;
    if !roots.is_undefined() {
        if roots.as_object().is_some_and(|o| o.is_array()) {
            for v in list_of(&roots, cx)? {
                ca.push(bytes(&v, cx)?);
            }
        } else {
            ca.push(bytes(&roots, cx)?);
        }
    }
    let mut alpn = Vec::new();
    let protocols = field("ALPNProtocols", cx)?;
    if let Some(raw) = crate::encoding::bytes_of(&protocols, cx)? {
        let mut i = 0;
        while i < raw.len() {
            let end = i + 1 + raw[i] as usize;
            if end > raw.len() {
                return Err(crate::native::type_error("Invalid ALPNProtocols"));
            }
            alpn.push(raw[i + 1..end].to_vec());
            i = end;
        }
    } else if protocols.is_object() {
        for v in list_of(&protocols, cx)? {
            alpn.push(text_of(&v, cx)?.into_bytes());
        }
    }
    let name = field("servername", cx)?;
    let reject = field("rejectUnauthorized", cx)?;
    let text = |v: JsValue| {
        if v.is_undefined() {
            String::new()
        } else {
            crate::print(&v)
        }
    };
    Ok(Options {
        host: if name.is_undefined() {
            host.into()
        } else {
            text_of(&name, cx)?
        },
        ca,
        cert: bytes(&field("cert", cx)?, cx)?,
        key: bytes(&field("key", cx)?, cx)?,
        alpn,
        reject: reject.is_undefined() || reject.to_boolean(),
        min: text(field("minVersion", cx)?),
        max: text(field("maxVersion", cx)?),
        ciphers: text(field("ciphers", cx)?),
        contexts: Vec::new(),
        request_cert: field("requestCert", cx)?.to_boolean(),
    })
}
fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}
fn certificates(pem: &[u8]) -> io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    rustls_pemfile::certs(&mut Cursor::new(pem)).collect::<Result<Vec<_>, _>>()
}
fn key(pem: &[u8]) -> io::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    rustls_pemfile::private_key(&mut Cursor::new(pem))?
        .ok_or_else(|| invalid("PEM private key is missing"))
}
fn roots(options: &Options) -> io::Result<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    if options.ca.is_empty() {
        for cert in rustls_native_certs::load_native_certs().certs {
            let _ = roots.add(cert);
        }
    } else {
        for pem in &options.ca {
            for cert in certificates(pem)? {
                roots.add(cert).map_err(invalid)?;
            }
        }
    }
    Ok(roots)
}
fn provider(options: &Options) -> Arc<rustls::crypto::CryptoProvider> {
    let mut provider = rustls::crypto::ring::default_provider();
    if !options.ciphers.is_empty() {
        let names = options.ciphers.to_ascii_uppercase();
        provider.cipher_suites.retain(|s| {
            names
                .split(':')
                .any(|name| name == format!("{:?}", s.suite()) || name == "DEFAULT")
        });
    }
    Arc::new(provider)
}
fn versions(options: &Options) -> io::Result<Vec<&'static rustls::SupportedProtocolVersion>> {
    let min = if options.min.is_empty() {
        "TLSv1.2"
    } else {
        options.min.as_str()
    };
    let max = if options.max.is_empty() {
        "TLSv1.3"
    } else {
        options.max.as_str()
    };
    if !["TLSv1.2", "TLSv1.3"].contains(&min) || !["TLSv1.2", "TLSv1.3"].contains(&max) || min > max
    {
        return Err(invalid("Unsupported TLS protocol version"));
    }
    let mut v = Vec::new();
    if min == "TLSv1.2" {
        v.push(&rustls::version::TLS12);
    }
    if max == "TLSv1.3" {
        v.push(&rustls::version::TLS13);
    }
    Ok(v)
}
#[derive(Debug)]
struct Unverified {
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl rustls::client::danger::ServerCertVerifier for Unverified {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
pub(crate) fn client(options: &Options) -> io::Result<rustls::Connection> {
    let provider = provider(options);
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&versions(options)?)
        .map_err(invalid)?;
    let builder = if options.reject {
        builder.with_root_certificates(roots(options)?)
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Unverified { provider }))
    };
    let mut config = if options.cert.is_empty() {
        builder.with_no_client_auth()
    } else {
        builder
            .with_client_auth_cert(certificates(&options.cert)?, key(&options.key)?)
            .map_err(invalid)?
    };
    config.alpn_protocols = options.alpn.clone();
    let name = rustls::pki_types::ServerName::try_from(options.host.clone()).map_err(invalid)?;
    Ok(rustls::ClientConnection::new(Arc::new(config), name)
        .map_err(invalid)?
        .into())
}
pub(crate) fn server(options: &Options) -> io::Result<rustls::Connection> {
    let provider = provider(options);
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&versions(options)?)
        .map_err(invalid)?;
    let builder = if options.request_cert {
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots(options)?));
        let verifier = if options.reject {
            verifier
        } else {
            verifier.allow_unauthenticated()
        };
        builder.with_client_cert_verifier(verifier.build().map_err(invalid)?)
    } else {
        builder.with_no_client_auth()
    };
    let mut config = if options.contexts.is_empty() {
        builder
            .with_single_cert(certificates(&options.cert)?, key(&options.key)?)
            .map_err(invalid)?
    } else {
        let mut resolver = rustls::server::ResolvesServerCertUsingSni::new();
        let signer = |option: &Options| -> io::Result<rustls::sign::CertifiedKey> {
            Ok(rustls::sign::CertifiedKey::new(
                certificates(&option.cert)?,
                provider
                    .key_provider
                    .load_private_key(key(&option.key)?)
                    .map_err(invalid)?,
            ))
        };
        resolver
            .add(&options.host, signer(options)?)
            .map_err(invalid)?;
        for (name, context) in &options.contexts {
            resolver.add(name, signer(context)?).map_err(invalid)?;
        }
        builder.with_cert_resolver(Arc::new(resolver))
    };
    config.alpn_protocols = options.alpn.clone();
    Ok(rustls::ServerConnection::new(Arc::new(config))
        .map_err(invalid)?
        .into())
}

fn init(target: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    tcp::init_socket(target, args, cx)?;
    target.set(js_string!("encrypted"), true, false, cx)?;
    target.set(js_string!("authorized"), false, false, cx)?;
    Ok(())
}
fn connect(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let Some((host, _, _)) = tcp::endpoint(args, false, cx)? else {
        return Err(crate::native::coded_error(
            cx,
            "IPC TLS sockets are unavailable",
            "ENOSYS",
        ));
    };
    let module = crate::modules::node_module(cx, "tls")?;
    let ctor = get(&module, "TLSSocket", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("TLSSocket"))?;
    let input = if arg(args, 0).is_object() {
        arg(args, 0)
    } else {
        args.iter()
            .find(|v| v.is_object() && !v.is_callable())
            .cloned()
            .unwrap_or_default()
    };
    let socket = ctor.construct(&[input.clone()], None, cx)?;
    let value: JsValue = socket.clone().into();
    if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
        invoke(&value, "once", &[js_text("secureConnect"), cb.clone()], cx)?;
    }
    let identity = get(&input, "checkServerIdentity", cx)?;
    hidden(&socket, js_string!("__tlsIdentity"), identity, cx)?;
    hidden(&socket, js_string!("__tlsOptions"), input.clone(), cx)?;
    let connect_args: Vec<_> = args.iter().filter(|v| !v.is_callable()).cloned().collect();
    tcp::connect(&value, &connect_args, cx)?;
    let id = super::id(&value, cx)?;
    if id != 0 {
        let config = options(&input, &host, cx)?;
        super::send(cx, Command::TlsClient(id, config));
    }
    Ok(value)
}
pub(crate) fn install(module: &JsObject, cx: &mut Context) -> JsResult<()> {
    let net = crate::modules::node_module(cx, "net")?;
    let parent = get(&net, "Socket", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("Socket"))?;
    let (socket, _) = class(
        cx,
        ClassSpec {
            name: "TLSSocket",
            parent: Some(&parent),
            init,
            methods: &[
                ("getPeerCertificate", 1, |this, _, cx| {
                    let cert = get(this, "__tlsCertificate", cx)?;
                    Ok(if cert.is_undefined() {
                        object(cx).into()
                    } else {
                        cert
                    })
                }),
                ("getProtocol", 0, |this, _, cx| {
                    get(this, "__tlsProtocol", cx)
                }),
                ("getCipher", 0, |this, _, cx| get(this, "__tlsCipher", cx)),
                ("getSession", 0, |_, _, _| Ok(JsValue::undefined())),
                ("isSessionReused", 0, |_, _, _| Ok(false.into())),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("TLSSocket"), socket, false, cx)?;
    let connect = function(cx, "connect", 3, connect);
    module.set(js_string!("connect"), connect, false, cx)?;
    let server_parent = get(&net, "Server", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("net.Server"))?;
    let (server, _) = class(
        cx,
        ClassSpec {
            name: "Server",
            parent: Some(&server_parent),
            init: |target, args, cx| {
                let options = arg(args, 0);
                tcp::init_server(target, &[options.clone()], cx)?;
                hidden(target, js_string!("__tlsOptions"), options, cx)?;
                if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
                    invoke(
                        &target.clone().into(),
                        "on",
                        &[js_text("secureConnection"), cb.clone()],
                        cx,
                    )?;
                }
                Ok(())
            },
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Server"), server.clone(), false, cx)?;
    let server: JsValue = server.into();
    if let Some(server) = server.as_object() {
        let proto = server.get(js_string!("prototype"), cx)?.as_object();
        if let Some(proto) = proto {
            let add = function(cx, "addContext", 2, |this, args, cx| {
                let contexts = get(this, "__tlsContexts", cx)?;
                let contexts = if let Some(o) = contexts.as_object() {
                    o
                } else {
                    let o = object(cx);
                    tcp::put(this, "__tlsContexts", o.clone(), cx)?;
                    o
                };
                contexts.set(
                    JsString::from(text_of(&arg(args, 0), cx)?),
                    arg(args, 1),
                    false,
                    cx,
                )?;
                Ok(JsValue::undefined())
            });
            proto.set(js_string!("addContext"), add, false, cx)?;
        }
    }
    let create = function(cx, "createServer", 2, |_, args, cx| {
        let net = crate::modules::node_module(cx, "tls")?;
        let ctor = get(&net, "Server", cx)?
            .as_object()
            .ok_or_else(|| crate::native::type_error("Server"))?;
        let options = arg(args, 0);
        let server = ctor.construct(&[options.clone()], None, cx)?;
        hidden(&server, js_string!("__tlsOptions"), options, cx)?;
        if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
            invoke(
                &server.clone().into(),
                "on",
                &[js_text("secureConnection"), cb.clone()],
                cx,
            )?;
        }
        Ok(server.into())
    });
    module.set(js_string!("createServer"), create, false, cx)?;
    let secure = function(cx, "createSecureContext", 1, |_, args, cx| {
        let context = object(cx);
        hidden(&context, js_string!("__tlsOptions"), arg(args, 0), cx)?;
        let inner = object(cx);
        context.set(js_string!("context"), inner, false, cx)?;
        Ok(context.into())
    });
    module.set(js_string!("createSecureContext"), secure, false, cx)?;
    Ok(())
}
pub(crate) fn accepted(
    server: &JsValue,
    socket: &JsValue,
    id: Id,
    cx: &mut Context,
) -> JsResult<bool> {
    let input = get(server, "__tlsOptions", cx)?;
    if input.is_undefined() {
        return Ok(false);
    }
    tcp::put(socket, "encrypted", true, cx)?;
    tcp::put(socket, "__tlsServer", true, cx)?;
    let mut config = options(&input, "localhost", cx)?;
    let contexts = get(server, "__tlsContexts", cx)?;
    if let Some(contexts) = contexts.as_object() {
        for name in contexts.own_property_keys(cx)? {
            let name = name.to_string();
            let value = contexts.get(JsString::from(name.as_str()), cx)?;
            config
                .contexts
                .push((name.clone(), options(&value, &name, cx)?));
        }
    }
    super::send(cx, Command::TlsServer(id, config));
    Ok(true)
}
pub(crate) fn peer_certificate(bytes: &[u8], cx: &mut Context) -> JsResult<JsObject> {
    use sha2::Digest;
    use x509_parser::prelude::FromDer;
    let peer = object(cx);
    let raw = crate::encoding::make_buffer(cx, bytes.to_vec())?;
    peer.set(js_string!("raw"), raw, false, cx)?;
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    };
    peer.set(
        js_string!("fingerprint"),
        js_text(&hex(&sha1::Sha1::digest(bytes))),
        false,
        cx,
    )?;
    peer.set(
        js_string!("fingerprint256"),
        js_text(&hex(&sha2::Sha256::digest(bytes))),
        false,
        cx,
    )?;
    peer.set(
        js_string!("fingerprint512"),
        js_text(&hex(&sha2::Sha512::digest(bytes))),
        false,
        cx,
    )?;
    if let Ok((_, cert)) = x509_parser::certificate::X509Certificate::from_der(bytes) {
        for (key, name) in [("subject", cert.subject()), ("issuer", cert.issuer())] {
            let o = object(cx);
            for attribute in name.iter_attributes() {
                let oid = attribute.attr_type().to_id_string();
                let name = match oid.as_str() {
                    "2.5.4.3" => "CN",
                    "2.5.4.6" => "C",
                    "2.5.4.7" => "L",
                    "2.5.4.8" => "ST",
                    "2.5.4.10" => "O",
                    "2.5.4.11" => "OU",
                    _ => oid.as_str(),
                };
                if let Ok(text) = attribute.as_str() {
                    o.set(JsString::from(name), js_text(text), false, cx)?;
                }
            }
            peer.set(JsString::from(key), o, false, cx)?;
        }
        peer.set(
            js_string!("serialNumber"),
            js_text(
                &cert
                    .raw_serial_as_string()
                    .replace(':', "")
                    .to_ascii_uppercase(),
            ),
            false,
            cx,
        )?;
        for (key, time) in [
            ("valid_from", cert.validity().not_before),
            ("valid_to", cert.validity().not_after),
        ] {
            let t = time.to_datetime();
            let months = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let month = months.get(t.month() as usize - 1).unwrap_or(&"Jan");
            peer.set(
                JsString::from(key),
                js_text(&format!(
                    "{month} {:2} {:02}:{:02}:{:02} {} GMT",
                    t.day(),
                    t.hour(),
                    t.minute(),
                    t.second(),
                    t.year()
                )),
                false,
                cx,
            )?;
        }
        if let Ok(Some(san)) = cert.subject_alternative_name() {
            let names = san
                .value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    x509_parser::extensions::GeneralName::DNSName(v) => Some(format!("DNS:{v}")),
                    x509_parser::extensions::GeneralName::URI(v) => Some(format!("URI:{v}")),
                    x509_parser::extensions::GeneralName::IPAddress(v) => {
                        let ip = if v.len() == 4 {
                            Some(std::net::IpAddr::from(<[u8; 4]>::try_from(*v).ok()?))
                        } else {
                            Some(std::net::IpAddr::from(<[u8; 16]>::try_from(*v).ok()?))
                        };
                        ip.map(|v| format!("IP Address:{v}"))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(", ");
            peer.set(js_string!("subjectaltname"), js_text(&names), false, cx)?;
        }
    }
    Ok(peer)
}

pub(crate) fn secured(
    target: &JsValue,
    protocol: String,
    alpn: Vec<u8>,
    cipher: String,
    cert: Vec<u8>,
    cx: &mut Context,
) -> JsResult<()> {
    tcp::put(target, "authorized", true, cx)?;
    tcp::put(
        target,
        "alpnProtocol",
        if alpn.is_empty() {
            false.into()
        } else {
            js_text(&String::from_utf8_lossy(&alpn))
        },
        cx,
    )?;
    tcp::put(target, "__tlsProtocol", js_text(&protocol), cx)?;
    let c = object(cx);
    c.set(js_string!("name"), js_text(&cipher), false, cx)?;
    c.set(js_string!("standardName"), js_text(&cipher), false, cx)?;
    c.set(js_string!("version"), js_text(&protocol), false, cx)?;
    tcp::put(target, "__tlsCipher", c, cx)?;
    let peer = peer_certificate(&cert, cx)?;
    tcp::put(target, "__tlsCertificate", peer.clone(), cx)?;
    let check = get(target, "__tlsIdentity", cx)?;
    if check.is_callable() {
        let host = get(target, "__networkHost", cx)?;
        let result = crate::call(cx, &check, &JsValue::undefined(), &[host, peer.into()])?;
        if result.is_object() {
            invoke(target, "destroy", &[result], cx)?;
            return Ok(());
        }
    }
    if get(target, "__tlsServer", cx)?.to_boolean() {
        let server = get(target, "server", cx)?;
        tcp::emit(&server, "secureConnection", &[target.clone()], cx)?;
    } else {
        tcp::emit(target, "secureConnect", &[], cx)?;
    }
    Ok(())
}
