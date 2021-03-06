#![deny(warnings)]

extern crate byteorder;
use byteorder::{BigEndian, WriteBytesExt};
#[macro_use]
extern crate failure;
use failure::Error;
extern crate openssl;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
extern crate solicit;
extern crate uuid;
use uuid::Uuid;

use openssl::crypto::pkey::PKey;
use openssl::ssl::SSL_OP_NO_COMPRESSION;
use openssl::ssl::SslMethod::Tlsv1_2;
use openssl::ssl::{Ssl, SslContext, SslStream};
use openssl::x509::X509;
use solicit::client::SimpleClient;
use solicit::http::ALPN_PROTOCOLS;
use solicit::http::{Header, HttpScheme};

use std::fs::File;
use std::io::BufReader;
use std::net::TcpStream;
use std::str;
use std::sync::Mutex;

mod types;
pub use types::*;

mod error;
pub use error::SendError;
use error::*;

pub struct APNs {
    gateway: String,
    ssl_context: SslContext,
}

impl APNs {
    pub fn new(cert_path: &str, key_path: &str, production: bool) -> Result<Self, Error> {
        let mut ctx = SslContext::new(Tlsv1_2)?;

        let cert_reader = &mut BufReader::new(File::open(cert_path)?);
        let x509 = X509::from_pem(cert_reader)?;
        let _ = ctx.set_certificate(&x509);

        let pkey_reader = &mut BufReader::new(File::open(key_path)?);
        let pkey = PKey::private_rsa_key_from_pem(pkey_reader)?;
        let _ = ctx.set_private_key(&pkey);

        ctx.set_options(SSL_OP_NO_COMPRESSION);
        ctx.set_alpn_protocols(ALPN_PROTOCOLS);
        ctx.set_npn_protocols(ALPN_PROTOCOLS);

        let gateway: String;
        if production {
            gateway = APN_URL_PRODUCTION.to_string();
        } else {
            gateway = APN_URL_DEV.to_string();
        }

        let apns = APNs {
            gateway: gateway,
            ssl_context: ctx,
        };
        Ok(apns)
    }

    pub fn new_client(&self) -> Result<APNsClient, Error> {
        let ssl = Ssl::new(&self.ssl_context)?;

        let raw_tcp = TcpStream::connect(self.gateway.as_str())?;
        let mut ssl_stream = SslStream::connect(ssl, raw_tcp)?;

        solicit::http::client::write_preface(&mut ssl_stream)?;

        Ok(APNsClient(Mutex::new(SimpleClient::with_stream(
            ssl_stream,
            self.gateway.clone(),
            HttpScheme::Https,
        )?)))
    }
}

pub struct APNsClient(Mutex<SimpleClient<SslStream<TcpStream>>>);

impl APNsClient {
    /// Send a notification.
    /// Returns the UUID (either the configured one, or the one returned by the
    /// api).
    pub fn send(&self, notification: Notification) -> Result<Uuid, SendError> {
        let n = notification;
        let path = format!("/3/device/{}", &n.device_token).into_bytes();

        // Just always generate a uuid client side for simplicity.
        let id = n.id.unwrap_or(Uuid::new_v4());

        let u32bytes = |i| {
            let mut wtr = vec![];
            wtr.write_u32::<BigEndian>(i).unwrap();
            wtr
        };
        let u64bytes = |i| {
            let mut wtr = vec![];
            wtr.write_u64::<BigEndian>(i).unwrap();
            wtr
        };

        let mut headers = Vec::new();
        headers.push(Header::new(
            b"apns-id".to_vec(),
            id.to_string().into_bytes(),
        ));
        headers.push(Header::new(b"apns-topic".to_vec(), n.topic.as_bytes()));
        n.expiration
            .map(|x| headers.push(Header::new(b"apns-expiration".to_vec(), u64bytes(x))));
        n.priority
            .map(|x| headers.push(Header::new(b"apns-priority".to_vec(), u32bytes(x.to_int()))));
        n.collapse_id.map(|x| {
            headers.push(Header::new(
                b"apns-collapse-id".to_vec(),
                x.as_str().to_string().into_bytes(),
            ))
        });

        let request = ApnsRequest {
            aps: n.payload,
            data: n.data,
        };
        let raw_request = ::serde_json::to_vec(&request)?;

        let post = self.0.lock().unwrap().post(&path, &headers, raw_request)?;

        let status = post.status_code()?;
        if status != 200 {
            // Request failed.
            // Read json response with the error.
            let reason = ErrorResponse::parse_payload(&post.body);
            Err(ApiError { status, reason }.into())
        } else {
            Ok(id)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::env::var;

    #[test]
    fn test_cert() {
        let cert_path = var("APNS_CERT_PATH").unwrap();
        let key_path = var("APNS_KEY_PATH").unwrap();
        let topic = var("APNS_TOPIC").unwrap();
        let token = var("APNS_DEVICE_TOKEN").unwrap();

        let apns = APNs::new(&cert_path, &key_path, false).unwrap();
        let mut apns_client = apns.new_client().unwrap();
        let n = NotificationBuilder::new(topic, token)
            .title("title")
            .body("body")
            .build();
        apns.send(n, &mut apns_client).unwrap();
    }
}
