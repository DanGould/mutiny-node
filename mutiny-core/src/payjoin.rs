use std::collections::HashMap;

use crate::error::MutinyError;
use crate::storage::MutinyStorage;
use hex_conservative::DisplayHex;
use once_cell::sync::Lazy;
use payjoin::receive::v2::Enrolled;
use payjoin::OhttpKeys;
use url::Url;

pub(crate) static OHTTP_RELAYS: [Lazy<Url>; 3] = [
    Lazy::new(|| Url::parse("https://ohttp.payjoin.org").expect("Invalid URL")),
    Lazy::new(|| Url::parse("https://ohttp-relay.obscuravpn.io").expect("Invalid URL")),
    Lazy::new(|| Url::parse("https://pj.bobspacebkk.com").expect("Invalid URL")),
];

pub(crate) static PAYJOIN_DIR: Lazy<Url> =
    Lazy::new(|| Url::parse("https://payjo.in").expect("Invalid URL"));

pub trait PayjoinStorage {
    fn get_payjoin(&self, id: &[u8; 33]) -> Result<Option<Enrolled>, MutinyError>;
    fn get_payjoins(&self) -> Result<Vec<Enrolled>, MutinyError>;
    fn persist_payjoin(&self, session: Enrolled) -> Result<(), MutinyError>;
}

const PAYJOIN_KEY_PREFIX: &str = "payjoin/";

fn get_payjoin_key(id: &[u8; 33]) -> String {
    format!("{PAYJOIN_KEY_PREFIX}{}", id.as_hex())
}

impl<S: MutinyStorage> PayjoinStorage for S {
    fn get_payjoin(&self, id: &[u8; 33]) -> Result<Option<Enrolled>, MutinyError> {
        let sessions = self.get_data(get_payjoin_key(id))?;
        Ok(sessions)
    }

    fn get_payjoins(&self) -> Result<Vec<Enrolled>, MutinyError> {
        let map: HashMap<String, Enrolled> = self.scan(PAYJOIN_KEY_PREFIX, None)?;
        Ok(map.values().map(|v| v.to_owned()).collect())
    }

    fn persist_payjoin(&self, session: Enrolled) -> Result<(), MutinyError> {
        self.set_data(get_payjoin_key(&session.pubkey()), session, None)
    }
}

pub async fn fetch_ohttp_keys(
    _ohttp_relay: Url,
    directory: Url,
) -> Result<OhttpKeys, Box<dyn std::error::Error>> {
    let http_client = reqwest::Client::builder().build()?;

    let ohttp_keys_res = http_client
        .get(format!("{}/ohttp-keys", directory.as_ref()))
        .send()
        .await?
        .bytes()
        .await?;
    Ok(OhttpKeys::decode(ohttp_keys_res.as_ref())?)
}

#[derive(Debug)]
pub enum Error {
    Reqwest(reqwest::Error),
    ReceiverStateMachine(String),
    Txid(bitcoin::hashes::hex::Error),
    Shutdown,
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self {
            Error::Reqwest(e) => write!(f, "Reqwest error: {}", e),
            Error::ReceiverStateMachine(e) => write!(f, "Payjoin state machine error: {}", e),
            Error::Txid(e) => write!(f, "Payjoin txid error: {}", e),
            Error::Shutdown => write!(f, "Payjoin stopped by application shutdown"),
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
