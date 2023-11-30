use crate::error::MutinyError;
use crate::storage::MutinyStorage;
use bitcoin::hashes::hex::ToHex;
use payjoin::receive::v2::Enrolled;
use std::collections::HashMap;

pub trait PayjoinStorage {
    fn get_payjoin(&self, id: &[u8; 33]) -> Result<Option<Enrolled>, MutinyError>;
    fn get_payjoins(&self) -> Result<Vec<Enrolled>, MutinyError>;
    fn persist_payjoin(&self, session: Enrolled) -> Result<(), MutinyError>;
}

const PAYJOIN_KEY_PREFIX: &str = "payjoin/";

fn get_payjoin_key(id: &[u8; 33]) -> String {
    format!("{PAYJOIN_KEY_PREFIX}{}", id.to_hex())
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
