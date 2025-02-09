pub mod message_handler;

use crate::error::MutinyError;
use crate::nodemanager::NodeIndex;
use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use aes::Aes256;
use bitcoin::bech32::{FromBase32, ToBase32, Variant};
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::{PublicKey, SecretKey};
use bitcoin::{bech32, secp256k1, OutPoint};
use cbc::{Decryptor, Encryptor};
use lightning::io::{Cursor, Read};
use lightning::ln::msgs::DecodeError;
use lightning::util::ser::{Readable, Writeable, Writer};
use std::collections::HashMap;
use std::fmt::Formatter;
use std::str::FromStr;

type Aes256CbcEnc = Encryptor<Aes256>;
type Aes256CbcDec = Decryptor<Aes256>;

pub const SCB_ENCRYPTION_KEY_DERIVATION_PATH: &str = "m/444'/444'/444'";

/// A static channel backup is a backup for the channels for a given node.
/// These are backups of the channel monitors, which store the necessary
/// information to recover the channel in case of a failure.
#[derive(Default, PartialEq, Eq, Clone)]
pub struct StaticChannelBackup {
    /// Map of the channel outpoint to the channel monitor
    /// This is a Vec<u8> because we can't implement Readable for ChannelMonitor
    /// without having a KeysManager, which we don't have here.
    pub(crate) monitors: HashMap<OutPoint, Vec<u8>>,
}

impl Writeable for StaticChannelBackup {
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), lightning::io::Error> {
        let len: u32 = self.monitors.len() as u32;
        writer.write_all(&len.to_be_bytes())?;
        for (outpoint, monitor) in self.monitors.iter() {
            writer.write_all(&outpoint.txid[..])?;
            writer.write_all(&outpoint.vout.to_be_bytes())?;
            let mon_len: u32 = monitor.len() as u32;
            writer.write_all(&mon_len.to_be_bytes())?;
            writer.write_all(monitor)?;
        }
        Ok(())
    }
}

impl Readable for StaticChannelBackup {
    fn read<R: Read>(reader: &mut R) -> Result<Self, DecodeError> {
        let len: u32 = Readable::read(reader)?;
        let mut monitors = HashMap::new();
        for _ in 0..len {
            let mut txid = [0u8; 32];
            reader.read_exact(&mut txid)?;
            let vout: u32 = Readable::read(reader)?;
            let outpoint = OutPoint {
                txid: bitcoin::Txid::from_slice(&txid).expect("txid is 32 bytes"),
                vout,
            };
            let mon_len: u32 = Readable::read(reader)?;
            let mut monitor = vec![0u8; mon_len as usize];
            reader.read_exact(&mut monitor)?;
            monitors.insert(outpoint, monitor);
        }

        Ok(Self { monitors })
    }
}

/// A static channel backup storage contains the static channel backups
/// for all of the node manager's nodes.
///
/// This also has the NodeStorage, which contains the the necessary
/// information to recover the node manager's nodes.
#[derive(Default, PartialEq, Eq, Clone)]
pub struct StaticChannelBackupStorage {
    pub(crate) backups: HashMap<PublicKey, (NodeIndex, StaticChannelBackup)>,
    pub(crate) peer_connections: HashMap<PublicKey, String>,
}

impl StaticChannelBackupStorage {
    pub(crate) fn encrypt(&self, secret_key: &SecretKey) -> EncryptedSCB {
        let bytes = self.encode();
        let iv: [u8; 16] = secp256k1::rand::random();

        let cipher = Aes256CbcEnc::new(&secret_key.secret_bytes().into(), &iv.into());
        let encrypted_scb: Vec<u8> = cipher.encrypt_padded_vec_mut::<Pkcs7>(&bytes);

        EncryptedSCB { encrypted_scb, iv }
    }
}

impl Writeable for StaticChannelBackupStorage {
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), lightning::io::Error> {
        // write backups
        let len: u32 = self.backups.len() as u32;
        writer.write_all(&len.to_be_bytes())?;
        for (public_key, (node_index, backup)) in self.backups.iter() {
            public_key.write(writer)?;
            node_index.write(writer)?;
            backup.write(writer)?;
        }

        // write peer connections
        let len: u32 = self.peer_connections.len() as u32;
        writer.write_all(&len.to_be_bytes())?;
        for (public_key, peer_connection) in self.peer_connections.iter() {
            writer.write_all(&public_key.serialize())?;
            let len: u32 = peer_connection.len() as u32;
            writer.write_all(&len.to_be_bytes())?;
            writer.write_all(peer_connection.as_bytes())?;
        }

        Ok(())
    }
}

impl Readable for StaticChannelBackupStorage {
    fn read<R: Read>(reader: &mut R) -> Result<Self, DecodeError> {
        // read backups
        let len: u32 = Readable::read(reader)?;
        let mut backups = HashMap::new();
        for _ in 0..len {
            let mut pk = [0u8; 33];
            reader.read_exact(&mut pk)?;
            let public_key = PublicKey::from_slice(&pk).expect("public key is 33 bytes");
            let node_index = Readable::read(reader)?;
            let backup = Readable::read(reader)?;
            backups.insert(public_key, (node_index, backup));
        }

        // read peer connections
        let len: u32 = Readable::read(reader)?;
        let mut peer_connections = HashMap::new();
        for _ in 0..len {
            // read public key
            let mut public_key = [0u8; 33];
            reader.read_exact(&mut public_key)?;
            let public_key = PublicKey::from_slice(&public_key).expect("public key is 33 bytes");

            // read peer connection
            let len: u32 = Readable::read(reader)?;
            let mut peer_connection = vec![0u8; len as usize];
            reader.read_exact(&mut peer_connection)?;
            let peer_connection =
                String::from_utf8(peer_connection).expect("peer connection is utf8");
            peer_connections.insert(public_key, peer_connection);
        }

        Ok(Self {
            backups,
            peer_connections,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EncryptedSCB {
    pub(crate) encrypted_scb: Vec<u8>,
    pub(crate) iv: [u8; 16],
}

impl EncryptedSCB {
    pub(crate) fn decrypt(
        &self,
        secret_key: &SecretKey,
    ) -> Result<StaticChannelBackupStorage, MutinyError> {
        let cipher =
            Aes256CbcDec::new(&secret_key.secret_bytes().into(), self.iv.as_slice().into());
        let result = cipher
            .decrypt_padded_vec_mut::<Pkcs7>(&self.encrypted_scb)
            .map_err(|_| MutinyError::InvalidMnemonic)?;

        let mut cursor = Cursor::new(result);
        Ok(StaticChannelBackupStorage::read(&mut cursor).expect("decoding succeeds"))
    }
}

impl Writeable for EncryptedSCB {
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), lightning::io::Error> {
        let len = self.encrypted_scb.len() as u32;
        writer.write_all(&len.to_be_bytes())?;
        writer.write_all(&self.encrypted_scb)?;
        writer.write_all(&self.iv)?;
        Ok(())
    }
}

impl Readable for EncryptedSCB {
    fn read<R: Read>(reader: &mut R) -> Result<Self, DecodeError> {
        let len: u32 = Readable::read(reader)?;
        let mut encrypted_scb = vec![0u8; len as usize];
        reader.read_exact(&mut encrypted_scb)?;
        let mut iv = [0u8; 16];
        reader.read_exact(&mut iv)?;
        Ok(Self { encrypted_scb, iv })
    }
}

impl FromStr for EncryptedSCB {
    type Err = DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (hrp, data, variant) = bech32::decode(s).map_err(|_| DecodeError::InvalidValue)?;
        if hrp != "scb" || variant != Variant::Bech32m {
            return Err(DecodeError::InvalidValue);
        }
        let bytes = Vec::<u8>::from_base32(&data).map_err(|_| DecodeError::InvalidValue)?;
        let mut reader = Cursor::new(bytes);
        Readable::read(&mut reader)
    }
}

impl core::fmt::Display for EncryptedSCB {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let bytes = self.encode();
        let s = bech32::encode("scb", bytes.to_base32(), Variant::Bech32m)
            .map_err(|_| std::fmt::Error)?;
        write!(f, "{}", s)
    }
}

#[cfg(test)]
mod test {
    use bitcoin::hashes::hex::FromHex;
    use std::str::FromStr;

    use super::*;

    // copied from ben's signet node
    const CHAIN_MONITOR_BYTES: [u8; 6082] = [
        1, 1, 0, 0, 0, 0, 0, 0, 0, 19, 84, 108, 201, 31, 61, 95, 0, 34, 81, 32, 142, 124, 212, 169,
        250, 144, 169, 131, 165, 4, 25, 54, 46, 161, 93, 129, 200, 231, 180, 98, 48, 45, 184, 196,
        39, 61, 130, 67, 109, 65, 227, 116, 1, 0, 22, 0, 20, 155, 222, 185, 143, 188, 53, 25, 92,
        149, 254, 213, 30, 138, 223, 241, 79, 173, 146, 163, 77, 0, 0, 0, 0, 0, 0, 52, 120, 216,
        192, 0, 0, 0, 0, 100, 95, 250, 103, 94, 78, 217, 42, 201, 139, 166, 56, 254, 143, 109, 53,
        105, 254, 162, 58, 3, 105, 127, 109, 187, 126, 86, 132, 89, 83, 177, 214, 31, 16, 112, 28,
        226, 21, 168, 45, 203, 113, 209, 36, 192, 124, 170, 74, 92, 228, 131, 56, 51, 3, 91, 63,
        84, 177, 231, 20, 166, 140, 69, 125, 12, 200, 201, 223, 172, 251, 11, 218, 152, 199, 244,
        168, 18, 131, 199, 246, 14, 17, 28, 11, 131, 0, 1, 0, 34, 0, 32, 38, 237, 112, 58, 24, 30,
        237, 250, 237, 9, 33, 36, 69, 140, 138, 151, 2, 93, 6, 83, 80, 165, 166, 32, 169, 232, 234,
        29, 202, 152, 236, 240, 33, 231, 40, 149, 61, 41, 196, 133, 164, 203, 219, 94, 139, 135,
        220, 58, 125, 254, 252, 146, 180, 231, 108, 235, 5, 89, 56, 167, 85, 206, 33, 2, 199, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 74, 0, 33, 3, 232, 31, 173, 69, 223, 41, 232, 17, 204, 51, 201, 212,
        6, 191, 62, 123, 27, 245, 72, 111, 57, 25, 165, 163, 96, 73, 102, 105, 82, 45, 209, 246, 2,
        33, 2, 168, 251, 56, 230, 219, 188, 53, 141, 74, 73, 184, 152, 41, 139, 190, 77, 95, 11, 1,
        255, 199, 30, 249, 190, 150, 196, 248, 171, 41, 236, 106, 151, 4, 2, 0, 144, 0, 71, 82, 33,
        2, 174, 247, 220, 89, 252, 184, 91, 73, 139, 10, 215, 68, 198, 31, 78, 110, 216, 242, 48,
        76, 240, 165, 163, 41, 225, 102, 10, 239, 218, 53, 131, 175, 33, 3, 162, 27, 136, 18, 216,
        238, 203, 44, 26, 39, 95, 143, 180, 119, 227, 27, 140, 224, 50, 72, 163, 59, 121, 67, 87,
        243, 47, 117, 76, 152, 191, 188, 82, 174, 0, 0, 0, 0, 0, 3, 3, 124, 255, 255, 255, 255,
        255, 248, 2, 161, 122, 34, 88, 88, 115, 156, 192, 54, 83, 178, 145, 51, 190, 82, 212, 61,
        142, 80, 60, 167, 127, 49, 146, 241, 49, 27, 50, 165, 87, 94, 116, 3, 161, 212, 121, 69,
        11, 17, 244, 126, 63, 53, 85, 1, 95, 194, 243, 104, 28, 140, 44, 42, 114, 160, 212, 146,
        28, 80, 8, 48, 132, 113, 41, 38, 0, 6, 5, 97, 253, 115, 220, 141, 11, 249, 39, 71, 67, 107,
        174, 33, 99, 43, 140, 28, 37, 51, 181, 81, 71, 50, 108, 236, 168, 2, 201, 148, 197, 188, 0,
        0, 255, 255, 255, 255, 255, 249, 113, 92, 161, 1, 27, 113, 98, 137, 76, 228, 64, 112, 198,
        93, 173, 69, 73, 205, 193, 190, 221, 122, 109, 87, 204, 48, 248, 147, 201, 88, 12, 174, 0,
        0, 255, 255, 255, 255, 255, 250, 169, 160, 4, 252, 0, 54, 199, 104, 97, 53, 164, 201, 112,
        199, 17, 190, 24, 246, 17, 235, 163, 236, 119, 120, 40, 101, 169, 121, 206, 255, 116, 164,
        0, 0, 255, 255, 255, 255, 255, 252, 73, 66, 208, 142, 9, 181, 4, 149, 154, 125, 220, 136,
        41, 63, 147, 82, 240, 230, 1, 142, 199, 44, 43, 104, 179, 197, 35, 82, 50, 120, 50, 69, 0,
        0, 255, 255, 255, 255, 255, 248, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9, 255, 237, 145, 119, 3, 153, 58,
        114, 100, 201, 138, 223, 195, 197, 142, 77, 206, 195, 212, 39, 32, 201, 203, 39, 122, 159,
        229, 72, 147, 162, 231, 222, 0, 0, 0, 0, 0, 0, 0, 0, 95, 43, 135, 240, 214, 37, 19, 67,
        212, 245, 49, 99, 97, 230, 200, 0, 107, 127, 198, 46, 204, 240, 87, 207, 72, 118, 87, 82,
        219, 135, 192, 193, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 49, 45, 0, 0, 1, 32, 15, 224,
        254, 193, 235, 125, 147, 116, 232, 8, 42, 87, 59, 41, 219, 6, 94, 231, 213, 20, 123, 227,
        124, 110, 28, 116, 165, 8, 218, 39, 30, 33, 34, 5, 0, 0, 0, 0, 0, 198, 68, 207, 98, 201,
        63, 203, 218, 59, 66, 192, 76, 115, 133, 213, 167, 184, 208, 36, 61, 185, 62, 116, 58, 186,
        134, 132, 126, 179, 37, 137, 5, 0, 0, 0, 0, 0, 0, 0, 0, 104, 191, 149, 151, 59, 250, 211,
        198, 14, 202, 9, 107, 158, 229, 222, 65, 14, 89, 51, 6, 65, 166, 132, 13, 7, 111, 228, 94,
        240, 169, 0, 171, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 5, 207, 187, 96, 0, 1, 31, 162,
        148, 14, 240, 130, 50, 175, 169, 1, 12, 76, 170, 175, 98, 217, 242, 135, 78, 214, 0, 107,
        80, 218, 93, 130, 11, 141, 95, 94, 69, 156, 151, 77, 5, 0, 0, 0, 0, 0, 199, 161, 78, 199,
        93, 110, 88, 179, 117, 22, 73, 124, 212, 198, 174, 40, 83, 219, 127, 50, 239, 84, 52, 228,
        52, 61, 202, 220, 198, 135, 220, 136, 0, 0, 0, 0, 0, 0, 0, 0, 197, 32, 174, 50, 130, 251,
        99, 172, 24, 6, 55, 64, 143, 231, 167, 82, 131, 91, 226, 146, 139, 173, 52, 243, 176, 117,
        214, 154, 27, 113, 63, 233, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, 134, 162, 0, 1,
        148, 49, 151, 105, 86, 178, 211, 204, 144, 93, 6, 119, 164, 22, 112, 234, 32, 92, 245, 89,
        184, 3, 208, 195, 111, 149, 7, 171, 38, 106, 65, 228, 176, 243, 0, 0, 231, 40, 149, 61, 41,
        196, 133, 164, 203, 219, 94, 139, 135, 220, 58, 125, 254, 252, 146, 180, 231, 108, 235, 5,
        89, 56, 167, 85, 206, 33, 2, 199, 0, 0, 0, 0, 0, 0, 0, 0, 86, 35, 207, 144, 10, 146, 58,
        14, 189, 35, 164, 134, 175, 164, 119, 82, 106, 94, 238, 70, 207, 169, 101, 236, 4, 63, 68,
        228, 27, 141, 50, 56, 0, 0, 0, 0, 0, 0, 0, 0, 162, 156, 225, 41, 78, 252, 113, 253, 205,
        135, 118, 236, 202, 49, 36, 2, 22, 31, 13, 59, 61, 23, 142, 158, 192, 176, 177, 116, 153,
        61, 192, 91, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 151,
        105, 86, 178, 211, 204, 144, 93, 6, 119, 164, 22, 112, 234, 32, 92, 245, 89, 184, 3, 208,
        195, 111, 149, 7, 171, 38, 106, 65, 228, 176, 243, 255, 255, 255, 255, 255, 248, 224, 254,
        193, 235, 125, 147, 116, 232, 8, 42, 87, 59, 41, 219, 6, 94, 231, 213, 20, 123, 227, 124,
        110, 28, 116, 165, 8, 218, 39, 30, 33, 34, 255, 255, 255, 255, 255, 252, 1, 253, 2, 5, 0,
        32, 49, 240, 100, 82, 27, 163, 141, 85, 206, 199, 74, 243, 47, 90, 198, 148, 198, 123, 139,
        126, 197, 12, 195, 102, 84, 119, 189, 26, 104, 33, 15, 22, 1, 8, 0, 0, 0, 0, 0, 1, 46, 87,
        2, 33, 2, 13, 204, 204, 146, 132, 154, 58, 52, 143, 63, 66, 75, 73, 185, 60, 80, 218, 38,
        150, 20, 44, 55, 73, 244, 116, 64, 147, 47, 202, 189, 145, 81, 4, 33, 2, 165, 213, 172,
        131, 140, 129, 114, 235, 165, 209, 86, 15, 226, 113, 202, 55, 156, 51, 215, 148, 62, 104,
        46, 87, 55, 206, 124, 105, 254, 114, 230, 30, 6, 33, 3, 182, 97, 224, 186, 138, 100, 120,
        82, 133, 21, 206, 153, 126, 188, 67, 116, 175, 129, 74, 31, 185, 235, 180, 212, 221, 185,
        120, 64, 247, 136, 61, 51, 8, 33, 2, 55, 239, 77, 147, 35, 131, 52, 11, 232, 186, 12, 195,
        36, 250, 188, 94, 59, 104, 28, 175, 111, 21, 97, 244, 27, 99, 246, 217, 243, 32, 158, 12,
        10, 33, 2, 132, 32, 27, 24, 128, 15, 129, 166, 136, 171, 66, 90, 4, 242, 231, 48, 219, 112,
        122, 54, 43, 49, 135, 26, 108, 155, 11, 220, 250, 165, 106, 10, 12, 4, 0, 0, 0, 253, 14,
        253, 1, 32, 53, 0, 1, 1, 2, 8, 0, 0, 0, 0, 0, 1, 134, 162, 4, 4, 0, 1, 148, 49, 6, 32, 151,
        105, 86, 178, 211, 204, 144, 93, 6, 119, 164, 22, 112, 234, 32, 92, 245, 89, 184, 3, 208,
        195, 111, 149, 7, 171, 38, 106, 65, 228, 176, 243, 0, 233, 0, 230, 0, 32, 21, 65, 6, 75,
        239, 109, 78, 196, 192, 64, 201, 52, 201, 32, 31, 78, 215, 225, 218, 6, 146, 177, 88, 156,
        37, 226, 208, 94, 35, 9, 109, 235, 1, 32, 151, 105, 86, 178, 211, 204, 144, 93, 6, 119,
        164, 22, 112, 234, 32, 92, 245, 89, 184, 3, 208, 195, 111, 149, 7, 171, 38, 106, 65, 228,
        176, 243, 2, 8, 0, 0, 0, 0, 0, 1, 134, 162, 4, 150, 76, 0, 33, 3, 102, 171, 200, 235, 77,
        166, 30, 49, 168, 210, 196, 82, 13, 49, 202, 189, 245, 140, 197, 37, 15, 133, 86, 87, 57,
        127, 61, 214, 36, 147, 147, 138, 2, 9, 0, 7, 8, 160, 0, 8, 10, 97, 162, 4, 8, 1, 31, 119,
        0, 0, 1, 0, 1, 6, 2, 0, 0, 8, 8, 0, 0, 0, 0, 0, 0, 0, 2, 10, 4, 0, 0, 0, 6, 72, 0, 33, 2,
        113, 169, 217, 106, 177, 69, 182, 8, 33, 106, 127, 97, 216, 0, 156, 161, 55, 68, 179, 30,
        10, 156, 41, 68, 79, 35, 71, 6, 110, 201, 235, 189, 2, 5, 0, 3, 2, 65, 0, 4, 8, 1, 110, 10,
        0, 0, 1, 0, 1, 6, 2, 0, 0, 8, 8, 0, 0, 0, 0, 0, 1, 134, 160, 10, 4, 0, 0, 0, 16, 227, 0,
        32, 241, 184, 113, 208, 25, 186, 69, 120, 220, 161, 215, 11, 64, 28, 139, 118, 45, 52, 8,
        182, 59, 39, 223, 208, 250, 40, 94, 26, 189, 26, 63, 217, 1, 8, 0, 0, 0, 0, 0, 1, 46, 87,
        2, 33, 2, 196, 247, 19, 178, 189, 35, 117, 137, 117, 98, 159, 126, 3, 117, 87, 242, 203, 0,
        86, 123, 142, 130, 143, 56, 17, 125, 89, 128, 11, 198, 244, 230, 4, 33, 3, 159, 32, 69,
        207, 222, 138, 17, 59, 253, 56, 153, 119, 105, 221, 41, 158, 204, 54, 248, 54, 128, 234,
        187, 58, 175, 33, 45, 43, 229, 136, 131, 168, 6, 33, 3, 186, 201, 174, 193, 25, 237, 117,
        250, 116, 242, 192, 125, 151, 166, 88, 123, 45, 143, 146, 9, 97, 190, 253, 48, 127, 108,
        241, 86, 118, 213, 164, 21, 8, 33, 3, 79, 164, 188, 23, 71, 134, 122, 118, 143, 14, 152, 3,
        82, 83, 1, 206, 129, 71, 154, 16, 46, 237, 227, 9, 160, 237, 8, 80, 42, 198, 38, 158, 10,
        33, 2, 139, 209, 73, 76, 152, 201, 148, 81, 186, 35, 121, 108, 97, 223, 101, 45, 135, 247,
        106, 137, 38, 231, 104, 19, 207, 205, 51, 255, 181, 191, 29, 195, 12, 4, 0, 0, 0, 253, 14,
        0, 255, 255, 255, 255, 255, 247, 255, 255, 255, 255, 255, 247, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 29, 242, 247, 60, 215, 119, 88, 209, 253, 69,
        200, 64, 175, 111, 20, 242, 192, 215, 89, 188, 214, 139, 79, 114, 50, 173, 94, 141, 186, 1,
        0, 0, 0, 1, 204, 31, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 3, 91, 63, 84, 177,
        231, 20, 166, 140, 69, 125, 12, 200, 201, 223, 172, 251, 11, 218, 152, 199, 244, 168, 18,
        131, 199, 246, 14, 17, 28, 11, 131, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 34, 0, 32, 38,
        237, 112, 58, 24, 30, 237, 250, 237, 9, 33, 36, 69, 140, 138, 151, 2, 93, 6, 83, 80, 165,
        166, 32, 169, 232, 234, 29, 202, 152, 236, 240, 1, 1, 0, 34, 81, 32, 142, 124, 212, 169,
        250, 144, 169, 131, 165, 4, 25, 54, 46, 161, 93, 129, 200, 231, 180, 98, 48, 45, 184, 196,
        39, 61, 130, 67, 109, 65, 227, 116, 253, 1, 202, 0, 253, 1, 127, 253, 1, 124, 0, 8, 0, 0,
        255, 255, 255, 255, 255, 247, 2, 8, 0, 0, 0, 0, 0, 1, 46, 87, 4, 8, 0, 0, 0, 0, 0, 1, 212,
        109, 6, 4, 0, 0, 0, 253, 8, 176, 175, 0, 33, 2, 139, 209, 73, 76, 152, 201, 148, 81, 186,
        35, 121, 108, 97, 223, 101, 45, 135, 247, 106, 137, 38, 231, 104, 19, 207, 205, 51, 255,
        181, 191, 29, 195, 2, 33, 2, 196, 247, 19, 178, 189, 35, 117, 137, 117, 98, 159, 126, 3,
        117, 87, 242, 203, 0, 86, 123, 142, 130, 143, 56, 17, 125, 89, 128, 11, 198, 244, 230, 4,
        33, 3, 159, 32, 69, 207, 222, 138, 17, 59, 253, 56, 153, 119, 105, 221, 41, 158, 204, 54,
        248, 54, 128, 234, 187, 58, 175, 33, 45, 43, 229, 136, 131, 168, 6, 33, 3, 186, 201, 174,
        193, 25, 237, 117, 250, 116, 242, 192, 125, 151, 166, 88, 123, 45, 143, 146, 9, 97, 190,
        253, 48, 127, 108, 241, 86, 118, 213, 164, 21, 8, 33, 3, 79, 164, 188, 23, 71, 134, 122,
        118, 143, 14, 152, 3, 82, 83, 1, 206, 129, 71, 154, 16, 46, 237, 227, 9, 160, 237, 8, 80,
        42, 198, 38, 158, 10, 162, 161, 0, 125, 2, 0, 0, 0, 1, 3, 91, 63, 84, 177, 231, 20, 166,
        140, 69, 125, 12, 200, 201, 223, 172, 251, 11, 218, 152, 199, 244, 168, 18, 131, 199, 246,
        14, 17, 28, 11, 131, 1, 0, 0, 0, 0, 201, 108, 84, 128, 2, 87, 46, 1, 0, 0, 0, 0, 0, 34, 0,
        32, 121, 4, 8, 56, 112, 165, 32, 207, 52, 110, 202, 192, 76, 171, 127, 31, 39, 162, 253,
        133, 80, 192, 90, 92, 197, 78, 18, 96, 204, 107, 182, 27, 109, 212, 1, 0, 0, 0, 0, 0, 22,
        0, 20, 126, 215, 199, 55, 174, 7, 223, 187, 142, 218, 19, 219, 229, 65, 0, 33, 28, 109,
        171, 161, 87, 61, 31, 32, 2, 32, 241, 184, 113, 208, 25, 186, 69, 120, 220, 161, 215, 11,
        64, 28, 139, 118, 45, 52, 8, 182, 59, 39, 223, 208, 250, 40, 94, 26, 189, 26, 63, 217, 12,
        0, 2, 64, 2, 12, 164, 132, 132, 228, 221, 238, 210, 6, 35, 12, 176, 93, 43, 213, 196, 15,
        55, 69, 134, 43, 87, 155, 3, 191, 83, 241, 102, 13, 0, 12, 95, 199, 149, 73, 39, 149, 112,
        107, 83, 4, 156, 180, 234, 46, 150, 28, 219, 78, 175, 166, 114, 75, 42, 146, 76, 250, 161,
        72, 85, 27, 219, 37, 4, 1, 1, 6, 0, 0, 253, 1, 206, 253, 1, 202, 0, 253, 1, 127, 253, 1,
        124, 0, 8, 0, 0, 255, 255, 255, 255, 255, 248, 2, 8, 0, 0, 0, 0, 0, 1, 46, 87, 4, 8, 0, 0,
        0, 0, 0, 1, 212, 9, 6, 4, 0, 0, 0, 253, 8, 176, 175, 0, 33, 2, 132, 32, 27, 24, 128, 15,
        129, 166, 136, 171, 66, 90, 4, 242, 231, 48, 219, 112, 122, 54, 43, 49, 135, 26, 108, 155,
        11, 220, 250, 165, 106, 10, 2, 33, 2, 13, 204, 204, 146, 132, 154, 58, 52, 143, 63, 66, 75,
        73, 185, 60, 80, 218, 38, 150, 20, 44, 55, 73, 244, 116, 64, 147, 47, 202, 189, 145, 81, 4,
        33, 2, 165, 213, 172, 131, 140, 129, 114, 235, 165, 209, 86, 15, 226, 113, 202, 55, 156,
        51, 215, 148, 62, 104, 46, 87, 55, 206, 124, 105, 254, 114, 230, 30, 6, 33, 3, 182, 97,
        224, 186, 138, 100, 120, 82, 133, 21, 206, 153, 126, 188, 67, 116, 175, 129, 74, 31, 185,
        235, 180, 212, 221, 185, 120, 64, 247, 136, 61, 51, 8, 33, 2, 55, 239, 77, 147, 35, 131,
        52, 11, 232, 186, 12, 195, 36, 250, 188, 94, 59, 104, 28, 175, 111, 21, 97, 244, 27, 99,
        246, 217, 243, 32, 158, 12, 10, 162, 161, 0, 125, 2, 0, 0, 0, 1, 3, 91, 63, 84, 177, 231,
        20, 166, 140, 69, 125, 12, 200, 201, 223, 172, 251, 11, 218, 152, 199, 244, 168, 18, 131,
        199, 246, 14, 17, 28, 11, 131, 1, 0, 0, 0, 0, 201, 108, 84, 128, 2, 87, 46, 1, 0, 0, 0, 0,
        0, 34, 0, 32, 198, 37, 89, 224, 68, 46, 142, 188, 87, 64, 199, 90, 168, 242, 233, 120, 232,
        96, 145, 101, 255, 238, 108, 124, 142, 234, 34, 45, 214, 249, 25, 117, 9, 212, 1, 0, 0, 0,
        0, 0, 22, 0, 20, 126, 215, 199, 55, 174, 7, 223, 187, 142, 218, 19, 219, 229, 65, 0, 33,
        28, 109, 171, 161, 88, 61, 31, 32, 2, 32, 49, 240, 100, 82, 27, 163, 141, 85, 206, 199, 74,
        243, 47, 90, 198, 148, 198, 123, 139, 126, 197, 12, 195, 102, 84, 119, 189, 26, 104, 33,
        15, 22, 12, 0, 2, 64, 22, 142, 97, 111, 80, 249, 58, 175, 176, 175, 27, 223, 48, 215, 234,
        54, 228, 170, 26, 162, 40, 27, 237, 94, 211, 216, 203, 28, 23, 210, 114, 247, 125, 142,
        170, 213, 34, 54, 101, 14, 245, 56, 4, 84, 212, 4, 168, 104, 199, 223, 253, 243, 37, 149,
        167, 118, 59, 230, 157, 171, 139, 66, 37, 174, 4, 1, 1, 6, 0, 0, 253, 1, 150, 0, 176, 175,
        0, 33, 2, 174, 247, 220, 89, 252, 184, 91, 73, 139, 10, 215, 68, 198, 31, 78, 110, 216,
        242, 48, 76, 240, 165, 163, 41, 225, 102, 10, 239, 218, 53, 131, 175, 2, 33, 3, 105, 127,
        109, 187, 126, 86, 132, 89, 83, 177, 214, 31, 16, 112, 28, 226, 21, 168, 45, 203, 113, 209,
        36, 192, 124, 170, 74, 92, 228, 131, 56, 51, 4, 33, 3, 99, 39, 136, 81, 8, 132, 11, 153,
        193, 161, 238, 86, 125, 98, 7, 84, 204, 79, 27, 32, 108, 150, 160, 201, 170, 160, 57, 246,
        122, 114, 95, 103, 6, 33, 2, 114, 36, 21, 35, 115, 142, 53, 162, 46, 222, 164, 31, 69, 122,
        201, 15, 99, 183, 225, 97, 243, 111, 55, 244, 173, 0, 30, 241, 39, 67, 243, 149, 8, 33, 3,
        150, 236, 223, 10, 147, 254, 123, 240, 35, 241, 141, 133, 110, 232, 252, 228, 103, 183, 51,
        120, 61, 52, 81, 58, 66, 4, 81, 180, 18, 139, 200, 80, 2, 2, 0, 144, 4, 1, 0, 6, 183, 182,
        0, 176, 175, 0, 33, 3, 162, 27, 136, 18, 216, 238, 203, 44, 26, 39, 95, 143, 180, 119, 227,
        27, 140, 224, 50, 72, 163, 59, 121, 67, 87, 243, 47, 117, 76, 152, 191, 188, 2, 33, 2, 177,
        103, 22, 140, 183, 178, 172, 122, 237, 205, 218, 248, 165, 127, 24, 117, 241, 85, 5, 118,
        146, 111, 199, 150, 132, 154, 51, 177, 104, 85, 36, 254, 4, 33, 2, 253, 113, 102, 60, 232,
        48, 49, 200, 173, 59, 12, 173, 40, 47, 172, 70, 139, 144, 74, 239, 135, 196, 99, 163, 236,
        112, 62, 77, 197, 236, 219, 72, 6, 33, 3, 232, 31, 173, 69, 223, 41, 232, 17, 204, 51, 201,
        212, 6, 191, 62, 123, 27, 245, 72, 111, 57, 25, 165, 163, 96, 73, 102, 105, 82, 45, 209,
        246, 8, 33, 2, 168, 251, 56, 230, 219, 188, 53, 141, 74, 73, 184, 152, 41, 139, 190, 77,
        95, 11, 1, 255, 199, 30, 249, 190, 150, 196, 248, 171, 41, 236, 106, 151, 2, 2, 0, 6, 8,
        34, 3, 91, 63, 84, 177, 231, 20, 166, 140, 69, 125, 12, 200, 201, 223, 172, 251, 11, 218,
        152, 199, 244, 168, 18, 131, 199, 246, 14, 17, 28, 11, 131, 0, 1, 0, 0, 2, 135, 1, 1, 100,
        193, 49, 143, 202, 163, 229, 165, 145, 255, 71, 104, 218, 39, 179, 28, 4, 146, 56, 105,
        224, 231, 239, 202, 181, 27, 237, 124, 73, 78, 27, 187, 120, 33, 74, 185, 111, 205, 228,
        245, 103, 73, 232, 24, 93, 64, 62, 223, 232, 190, 86, 248, 90, 119, 31, 120, 255, 194, 111,
        167, 73, 252, 230, 62, 44, 212, 4, 219, 114, 49, 209, 195, 158, 32, 249, 241, 223, 151,
        199, 166, 73, 102, 177, 148, 71, 188, 125, 176, 120, 94, 191, 71, 241, 126, 167, 0, 105,
        70, 135, 88, 254, 0, 138, 62, 246, 87, 188, 221, 217, 232, 6, 30, 208, 154, 0, 115, 12,
        206, 242, 6, 44, 78, 53, 229, 152, 212, 142, 181, 252, 153, 213, 13, 118, 235, 143, 129,
        213, 17, 9, 17, 66, 3, 226, 26, 130, 104, 93, 7, 3, 221, 212, 231, 28, 251, 216, 13, 35,
        239, 114, 109, 18, 226, 161, 246, 240, 3, 72, 16, 229, 248, 206, 46, 233, 87, 210, 7, 102,
        214, 105, 204, 161, 245, 40, 220, 223, 88, 44, 43, 87, 9, 225, 63, 253, 1, 154, 253, 1,
        150, 0, 176, 175, 0, 33, 2, 174, 247, 220, 89, 252, 184, 91, 73, 139, 10, 215, 68, 198, 31,
        78, 110, 216, 242, 48, 76, 240, 165, 163, 41, 225, 102, 10, 239, 218, 53, 131, 175, 2, 33,
        3, 105, 127, 109, 187, 126, 86, 132, 89, 83, 177, 214, 31, 16, 112, 28, 226, 21, 168, 45,
        203, 113, 209, 36, 192, 124, 170, 74, 92, 228, 131, 56, 51, 4, 33, 3, 99, 39, 136, 81, 8,
        132, 11, 153, 193, 161, 238, 86, 125, 98, 7, 84, 204, 79, 27, 32, 108, 150, 160, 201, 170,
        160, 57, 246, 122, 114, 95, 103, 6, 33, 2, 114, 36, 21, 35, 115, 142, 53, 162, 46, 222,
        164, 31, 69, 122, 201, 15, 99, 183, 225, 97, 243, 111, 55, 244, 173, 0, 30, 241, 39, 67,
        243, 149, 8, 33, 3, 150, 236, 223, 10, 147, 254, 123, 240, 35, 241, 141, 133, 110, 232,
        252, 228, 103, 183, 51, 120, 61, 52, 81, 58, 66, 4, 81, 180, 18, 139, 200, 80, 2, 2, 0,
        144, 4, 1, 0, 6, 183, 182, 0, 176, 175, 0, 33, 3, 162, 27, 136, 18, 216, 238, 203, 44, 26,
        39, 95, 143, 180, 119, 227, 27, 140, 224, 50, 72, 163, 59, 121, 67, 87, 243, 47, 117, 76,
        152, 191, 188, 2, 33, 2, 177, 103, 22, 140, 183, 178, 172, 122, 237, 205, 218, 248, 165,
        127, 24, 117, 241, 85, 5, 118, 146, 111, 199, 150, 132, 154, 51, 177, 104, 85, 36, 254, 4,
        33, 2, 253, 113, 102, 60, 232, 48, 49, 200, 173, 59, 12, 173, 40, 47, 172, 70, 139, 144,
        74, 239, 135, 196, 99, 163, 236, 112, 62, 77, 197, 236, 219, 72, 6, 33, 3, 232, 31, 173,
        69, 223, 41, 232, 17, 204, 51, 201, 212, 6, 191, 62, 123, 27, 245, 72, 111, 57, 25, 165,
        163, 96, 73, 102, 105, 82, 45, 209, 246, 8, 33, 2, 168, 251, 56, 230, 219, 188, 53, 141,
        74, 73, 184, 152, 41, 139, 190, 77, 95, 11, 1, 255, 199, 30, 249, 190, 150, 196, 248, 171,
        41, 236, 106, 151, 2, 2, 0, 6, 8, 34, 3, 91, 63, 84, 177, 231, 20, 166, 140, 69, 125, 12,
        200, 201, 223, 172, 251, 11, 218, 152, 199, 244, 168, 18, 131, 199, 246, 14, 17, 28, 11,
        131, 0, 1, 0, 0, 0, 0, 0, 3, 3, 124, 0, 0, 0, 0, 52, 120, 216, 192, 0, 0, 0, 0, 100, 95,
        250, 103, 94, 78, 217, 42, 201, 139, 166, 56, 254, 143, 109, 53, 105, 254, 162, 58, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 48, 3, 0, 5, 0, 7, 1, 0, 9, 33, 3, 102, 171, 200, 235, 77, 166, 30, 49, 168,
        210, 196, 82, 13, 49, 202, 189, 245, 140, 197, 37, 15, 133, 86, 87, 57, 127, 61, 214, 36,
        147, 147, 138, 13, 0, 15, 2, 0, 0,
    ];

    #[test]
    fn test_empty_static_channel_backup() {
        let backup = StaticChannelBackup::default();
        let node_index = NodeIndex {
            child_index: 0,
            lsp: None,
            archived: Some(false),
        };

        let pk = PublicKey::from_str(
            "02cae09cf2c8842ace44068a5bf3117a494ebbf69a99e79712483c36f97cdb7b54",
        )
        .unwrap();
        let storage = StaticChannelBackupStorage {
            backups: vec![(pk, (node_index.clone(), backup))]
                .into_iter()
                .collect(),
            peer_connections: HashMap::new(),
        };

        let bytes = storage.encode();

        let mut buffer = Cursor::new(&bytes);
        let decoded = StaticChannelBackupStorage::read(&mut buffer).unwrap();

        assert_eq!(decoded.backups.len(), 1);
        assert_eq!(decoded.backups.get(&pk).unwrap().0, node_index);
        assert_eq!(
            decoded.backups.get(&pk).unwrap().1.encode(),
            StaticChannelBackup::default().encode()
        );
        assert_eq!(decoded.backups.get(&pk).unwrap().1.monitors.len(), 0);
        assert_eq!(decoded.encode(), storage.encode());
    }

    #[test]
    fn test_empty_static_channel_backup_storage() {
        let storage = StaticChannelBackupStorage::default();
        let bytes = storage.encode();

        let mut buffer = Cursor::new(&bytes);
        let decoded = StaticChannelBackupStorage::read(&mut buffer).unwrap();

        assert_eq!(decoded.backups.len(), 0);
        assert_eq!(
            decoded.encode(),
            StaticChannelBackupStorage::default().encode()
        );
    }

    #[test]
    fn test_static_channel_backup() {
        let outpoint = OutPoint {
            txid: bitcoin::Txid::from_hex(
                "830b1c110ef6c78312a8f4c798da0bfbacdfc9c80c7d458ca614e7b1543f5b03",
            )
            .unwrap(),
            vout: 1,
        };

        let backup = StaticChannelBackup {
            monitors: vec![(outpoint, CHAIN_MONITOR_BYTES.to_vec())]
                .into_iter()
                .collect(),
        };

        let backup_bytes = backup.encode();
        let read = StaticChannelBackup::read(&mut Cursor::new(&backup_bytes)).unwrap();

        assert!(read == backup);
    }

    #[test]
    fn test_static_channel_backup_storage() {
        let outpoint = OutPoint {
            txid: bitcoin::Txid::from_hex(
                "830b1c110ef6c78312a8f4c798da0bfbacdfc9c80c7d458ca614e7b1543f5b03",
            )
            .unwrap(),
            vout: 1,
        };

        let pubkey = PublicKey::from_str(
            "02cae09cf2c8842ace44068a5bf3117a494ebbf69a99e79712483c36f97cdb7b54",
        )
        .unwrap();

        let connection_str =
            "02cae09cf2c8842ace44068a5bf3117a494ebbf69a99e79712483c36f97cdb7b54@192.168.0.1:9735"
                .to_string();

        let backup = StaticChannelBackup {
            monitors: vec![(outpoint, CHAIN_MONITOR_BYTES.to_vec())]
                .into_iter()
                .collect(),
        };

        let node_index = NodeIndex {
            child_index: 0,
            lsp: Some("https://signet-lsp.mutinywallet.com".to_string()),
            archived: Some(false),
        };

        let storage = StaticChannelBackupStorage {
            backups: vec![(pubkey, (node_index, backup))].into_iter().collect(),
            peer_connections: vec![(pubkey, connection_str)].into_iter().collect(),
        };

        let storage_bytes = storage.encode();
        let read = StaticChannelBackupStorage::read(&mut Cursor::new(&storage_bytes)).unwrap();

        assert!(read == storage);
    }

    #[test]
    fn test_encrypted_static_channel_backup_storage() {
        let outpoint = OutPoint {
            txid: bitcoin::Txid::from_hex(
                "830b1c110ef6c78312a8f4c798da0bfbacdfc9c80c7d458ca614e7b1543f5b03",
            )
            .unwrap(),
            vout: 1,
        };

        let pubkey = PublicKey::from_str(
            "02cae09cf2c8842ace44068a5bf3117a494ebbf69a99e79712483c36f97cdb7b54",
        )
        .unwrap();

        let connection_str =
            "02cae09cf2c8842ace44068a5bf3117a494ebbf69a99e79712483c36f97cdb7b54@192.168.0.1:9735"
                .to_string();

        let backup = StaticChannelBackup {
            monitors: vec![(outpoint, CHAIN_MONITOR_BYTES.to_vec())]
                .into_iter()
                .collect(),
        };

        let node_index = NodeIndex {
            child_index: 0,
            lsp: Some("https://signet-lsp.mutinywallet.com".to_string()),
            archived: Some(false),
        };

        let storage = StaticChannelBackupStorage {
            backups: vec![(pubkey, (node_index, backup))].into_iter().collect(),
            peer_connections: vec![(pubkey, connection_str)].into_iter().collect(),
        };

        // gen key
        let mut bytes = [0u8; 32];
        getrandom::getrandom(&mut bytes).expect("Failed to generate entropy");
        let encryption_key = SecretKey::from_slice(&bytes).unwrap();

        let encrypted = storage.encrypt(&encryption_key);
        assert!(encrypted == EncryptedSCB::from_str(&encrypted.to_string()).unwrap());

        // decrypt
        let decrypted = encrypted.decrypt(&encryption_key).unwrap();
        assert!(decrypted == storage);
    }
}
