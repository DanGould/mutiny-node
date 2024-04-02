use std::collections::HashMap;

use crate::error::MutinyError;
use crate::storage::MutinyStorage;
use core::time::Duration;
use hex_conservative::DisplayHex;
use once_cell::sync::Lazy;
use payjoin::receive::v2::Enrolled;
use payjoin::OhttpKeys;
use serde::{Deserialize, Serialize};
use url::Url;

pub(crate) static OHTTP_RELAYS: [Lazy<Url>; 3] = [
    Lazy::new(|| Url::parse("https://ohttp.payjoin.org").expect("Invalid URL")),
    Lazy::new(|| Url::parse("https://ohttp-relay.obscuravpn.io").expect("Invalid URL")),
    Lazy::new(|| Url::parse("https://pj.bobspacebkk.com").expect("Invalid URL")),
];

pub(crate) static PAYJOIN_DIR: Lazy<Url> =
    Lazy::new(|| Url::parse("https://payjo.in").expect("Invalid URL"));

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub enrolled: Enrolled,
    pub expiry: Duration,
}

impl Session {
    pub fn pubkey(&self) -> [u8; 33] {
        self.enrolled.pubkey()
    }
}
pub trait PayjoinStorage {
    fn get_payjoin(&self, id: &[u8; 33]) -> Result<Option<Session>, MutinyError>;
    fn get_payjoins(&self) -> Result<Vec<Session>, MutinyError>;
    fn persist_payjoin(&self, session: Enrolled) -> Result<Session, MutinyError>;
    fn delete_payjoin(&self, id: &[u8; 33]) -> Result<(), MutinyError>;
}

const PAYJOIN_KEY_PREFIX: &str = "payjoin/";

fn get_payjoin_key(id: &[u8; 33]) -> String {
    format!("{PAYJOIN_KEY_PREFIX}{}", id.as_hex())
}

impl<S: MutinyStorage> PayjoinStorage for S {
    fn get_payjoin(&self, id: &[u8; 33]) -> Result<Option<Session>, MutinyError> {
        let sessions = self.get_data(get_payjoin_key(id))?;
        Ok(sessions)
    }

    fn get_payjoins(&self) -> Result<Vec<Session>, MutinyError> {
        let map: HashMap<String, Session> = self.scan(PAYJOIN_KEY_PREFIX, None)?;
        Ok(map.values().map(|v| v.to_owned()).collect())
    }

    fn persist_payjoin(&self, enrolled: Enrolled) -> Result<Session, MutinyError> {
        let in_24_hours = crate::utils::now() + Duration::from_secs(60 * 60 * 24);
        let session = Session {
            enrolled,
            expiry: in_24_hours,
        };
        self.set_data(get_payjoin_key(&session.pubkey()), session.clone(), None)
            .map(|_| session)
    }

    fn delete_payjoin(&self, id: &[u8; 33]) -> Result<(), MutinyError> {
        self.delete(&[get_payjoin_key(id)])
    }
}

pub async fn fetch_ohttp_keys(_ohttp_relay: Url, directory: Url) -> Result<OhttpKeys, Error> {
    let http_client = reqwest::Client::builder().build()?;

    let ohttp_keys_res = http_client
        .get(format!("{}/ohttp-keys", directory.as_ref()))
        .send()
        .await?
        .bytes()
        .await?;
    Ok(OhttpKeys::decode(ohttp_keys_res.as_ref()).map_err(|_| Error::OhttpDecodeFailed)?)
}

#[derive(Debug)]
pub enum Error {
    Reqwest(reqwest::Error),
    ReceiverStateMachine(String),
    Txid(bitcoin::hashes::hex::Error),
    OhttpDecodeFailed,
    Shutdown,
    SessionExpired,
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self {
            Error::Reqwest(e) => write!(f, "Reqwest error: {}", e),
            Error::ReceiverStateMachine(e) => write!(f, "Payjoin state machine error: {}", e),
            Error::Txid(e) => write!(f, "Payjoin txid error: {}", e),
            Error::OhttpDecodeFailed => write!(f, "Failed to decode ohttp keys"),
            Error::Shutdown => write!(f, "Payjoin stopped by application shutdown"),
            Error::SessionExpired => write!(f, "Payjoin session expired. Create a new payment request and have the sender try again."),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Reqwest(e)
    }
}

impl From<payjoin::receive::Error> for Error {
    fn from(e: payjoin::receive::Error) -> Self {
        Error::ReceiverStateMachine(e.to_string())
    }
}
