use crate::error::MutinyError;
use crate::storage::MutinyStorage;
use bitcoin::hashes::hex::ToHex;
use core::time::Duration;
use payjoin::receive::v2::Enrolled;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(crate) const OHTTP_RELAYS: [&str; 2] = [
    "https://ohttp-relay.obscuravpn.io/payjoin",
    "https://bobspace-ohttp.duckdns.org",
];
pub(crate) const PAYJOIN_DIR: &str = "https://payjo.in";

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
    format!("{PAYJOIN_KEY_PREFIX}{}", id.to_hex())
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
