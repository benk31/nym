// Copyright 2020 Nym Technologies SA
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::authentication::encrypted_address::EncryptedAddressBytes;
use crate::authentication::iv::AuthenticationIV;
use crate::registration::handshake::SharedKeys;
use crate::GatewayMacSize;
use crypto::generic_array::typenum::Unsigned;
use crypto::hmac::recompute_keyed_hmac_and_verify_tag;
use crypto::symmetric::stream_cipher;
use nymsphinx::addressing::nodes::{NymNodeRoutingAddress, NymNodeRoutingAddressError};
use nymsphinx::params::packet_sizes::PacketSize;
use nymsphinx::params::{GatewayEncryptionAlgorithm, GatewayIntegrityHmacAlgorithm};
use nymsphinx::{DestinationAddressBytes, SphinxPacket};
use serde::{Deserialize, Serialize};
use std::{
    convert::{TryFrom, TryInto},
    fmt::{self, Error, Formatter},
};
use tungstenite::protocol::Message;

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RegistrationHandshake {
    HandshakePayload { data: Vec<u8> },
    HandshakeError { message: String },
}

impl RegistrationHandshake {
    pub fn new_payload(data: Vec<u8>) -> Self {
        RegistrationHandshake::HandshakePayload { data }
    }

    pub fn new_error<S: Into<String>>(message: S) -> Self {
        RegistrationHandshake::HandshakeError {
            message: message.into(),
        }
    }
}

impl TryFrom<String> for RegistrationHandshake {
    type Error = serde_json::Error;

    fn try_from(msg: String) -> Result<Self, serde_json::Error> {
        serde_json::from_str(&msg)
    }
}

impl TryInto<String> for RegistrationHandshake {
    type Error = serde_json::Error;

    fn try_into(self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self)
    }
}

#[derive(Debug)]
pub enum GatewayRequestsError {
    TooShortRequest,
    InvalidMAC,
    IncorrectlyEncodedAddress,
    RequestOfInvalidSize(usize),
    MalformedSphinxPacket,
    MalformedEncryption,
}

// to use it as `std::error::Error`, and we don't want to just derive is because we want
// the message to convey meanings of the usize tuple in RequestOfInvalidSize.
impl fmt::Display for GatewayRequestsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        use GatewayRequestsError::*;
        match self {
            TooShortRequest => write!(f, "the request is too short"),
            InvalidMAC => write!(f, "provided MAC is invalid"),
            IncorrectlyEncodedAddress => write!(f, "address field was incorrectly encoded"),
            RequestOfInvalidSize(actual) =>
                write!(
                f,
                "received request had invalid size. (actual: {}, but expected one of: {} (ACK), {} (REGULAR), {} (EXTENDED))",
                actual, PacketSize::ACKPacket.size(), PacketSize::RegularPacket.size(), PacketSize::ExtendedPacket.size()
            ),
            MalformedSphinxPacket => write!(f, "received sphinx packet was malformed"),
            MalformedEncryption => write!(f, "the received encrypted data was malformed"),
        }
    }
}

impl From<NymNodeRoutingAddressError> for GatewayRequestsError {
    fn from(_: NymNodeRoutingAddressError) -> Self {
        GatewayRequestsError::IncorrectlyEncodedAddress
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClientControlRequest {
    // TODO: should this also contain a MAC considering that at this point we already
    // have the shared key derived?
    Authenticate {
        address: String,
        enc_address: String,
        iv: String,
    },
    #[serde(alias = "handshakePayload")]
    RegisterHandshakeInitRequest { data: Vec<u8> },
}

impl ClientControlRequest {
    pub fn new_authenticate(
        address: DestinationAddressBytes,
        enc_address: EncryptedAddressBytes,
        iv: AuthenticationIV,
    ) -> Self {
        ClientControlRequest::Authenticate {
            address: address.to_base58_string(),
            enc_address: enc_address.to_base58_string(),
            iv: iv.to_base58_string(),
        }
    }
}

impl Into<Message> for ClientControlRequest {
    fn into(self) -> Message {
        // it should be safe to call `unwrap` here as the message is generated by the server
        // so if it fails (and consequently panics) it's a bug that should be resolved
        let str_req = serde_json::to_string(&self).unwrap();
        Message::Text(str_req)
    }
}

impl TryFrom<String> for ClientControlRequest {
    type Error = serde_json::Error;

    fn try_from(msg: String) -> Result<Self, Self::Error> {
        serde_json::from_str(&msg)
    }
}

impl TryInto<String> for ClientControlRequest {
    type Error = serde_json::Error;

    fn try_into(self) -> Result<String, Self::Error> {
        serde_json::to_string(&self)
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerResponse {
    Authenticate { status: bool },
    Register { status: bool },
    Send { status: bool },
    Error { message: String },
}

impl ServerResponse {
    pub fn new_error<S: Into<String>>(msg: S) -> Self {
        ServerResponse::Error {
            message: msg.into(),
        }
    }

    pub fn is_error(&self) -> bool {
        match self {
            ServerResponse::Error { .. } => true,
            _ => false,
        }
    }

    pub fn implies_successful_authentication(&self) -> bool {
        match self {
            ServerResponse::Authenticate { status, .. } => *status,
            ServerResponse::Register { status, .. } => *status,
            _ => false,
        }
    }
}

impl Into<Message> for ServerResponse {
    fn into(self) -> Message {
        // it should be safe to call `unwrap` here as the message is generated by the server
        // so if it fails (and consequently panics) it's a bug that should be resolved
        let str_res = serde_json::to_string(&self).unwrap();
        Message::Text(str_res)
    }
}

impl TryFrom<String> for ServerResponse {
    type Error = serde_json::Error;

    fn try_from(msg: String) -> Result<Self, serde_json::Error> {
        serde_json::from_str(&msg)
    }
}

pub enum BinaryRequest {
    ForwardSphinx {
        address: NymNodeRoutingAddress,
        sphinx_packet: SphinxPacket,
    },
}

// Right now the only valid `BinaryRequest` is a request to forward a sphinx packet.
// It is encrypted using the derived shared key between client and the gateway. Thanks to
// randomness inside the sphinx packet themselves (even via the same route), the 0s IV can be used here.
// HOWEVER, NOTE: If we introduced another 'BinaryRequest', we must carefully examine if a 0s IV
// would work there.
impl BinaryRequest {
    pub fn try_from_encrypted_tagged_bytes(
        mut raw_req: Vec<u8>,
        shared_keys: &SharedKeys,
    ) -> Result<Self, GatewayRequestsError> {
        let mac_size = GatewayMacSize::to_usize();
        if raw_req.len() < mac_size {
            return Err(GatewayRequestsError::TooShortRequest);
        }

        let mac_tag = &raw_req[..mac_size];
        let message_bytes = &raw_req[mac_size..];

        if !recompute_keyed_hmac_and_verify_tag::<GatewayIntegrityHmacAlgorithm>(
            shared_keys.mac_key(),
            message_bytes,
            mac_tag,
        ) {
            return Err(GatewayRequestsError::InvalidMAC);
        }

        // couldn't have made the first borrow mutable as you can't have an immutable borrow
        // together with a mutable one
        let mut message_bytes_mut = &mut raw_req[mac_size..];

        let zero_iv = stream_cipher::zero_iv::<GatewayEncryptionAlgorithm>();
        stream_cipher::decrypt_in_place::<GatewayEncryptionAlgorithm>(
            shared_keys.encryption_key(),
            &zero_iv,
            &mut message_bytes_mut,
        );

        // right now there's only a single option possible which significantly simplifies the logic
        // if we decided to allow for more 'binary' messages, the API wouldn't need to change
        let address = NymNodeRoutingAddress::try_from_bytes(&message_bytes_mut)?;
        let addr_offset = address.bytes_min_len();

        let sphinx_packet_data = &message_bytes_mut[addr_offset..];
        let packet_size = sphinx_packet_data.len();
        if PacketSize::get_type(packet_size).is_err() {
            // TODO: should this allow AckPacket sizes?

            Err(GatewayRequestsError::RequestOfInvalidSize(packet_size))
        } else {
            let sphinx_packet = match SphinxPacket::from_bytes(sphinx_packet_data) {
                Ok(packet) => packet,
                Err(_) => return Err(GatewayRequestsError::MalformedSphinxPacket),
            };

            Ok(BinaryRequest::ForwardSphinx {
                address,
                sphinx_packet,
            })
        }
    }

    pub fn into_encrypted_tagged_bytes(self, shared_key: &SharedKeys) -> Vec<u8> {
        match self {
            BinaryRequest::ForwardSphinx {
                address,
                sphinx_packet,
            } => {
                let forwarding_data: Vec<_> = address
                    .as_bytes()
                    .into_iter()
                    .chain(sphinx_packet.to_bytes().into_iter())
                    .collect();

                // TODO: it could be theoretically slightly more efficient if the data wasn't taken
                // by reference because then it makes a copy for encryption rather than do it in place
                shared_key.encrypt_and_tag(&forwarding_data, None)
            }
        }
    }

    // TODO: this will be encrypted, etc.
    pub fn new_forward_request(
        address: NymNodeRoutingAddress,
        sphinx_packet: SphinxPacket,
    ) -> BinaryRequest {
        BinaryRequest::ForwardSphinx {
            address,
            sphinx_packet,
        }
    }

    pub fn into_ws_message(self, shared_key: &SharedKeys) -> Message {
        Message::Binary(self.into_encrypted_tagged_bytes(shared_key))
    }
}

// Introduced for consistency sake
pub enum BinaryResponse {
    PushedMixMessage(Vec<u8>),
}

impl BinaryResponse {
    pub fn try_from_encrypted_tagged_bytes(
        raw_req: Vec<u8>,
        shared_keys: &SharedKeys,
    ) -> Result<Self, GatewayRequestsError> {
        let mac_size = GatewayMacSize::to_usize();
        if raw_req.len() < mac_size {
            return Err(GatewayRequestsError::TooShortRequest);
        }

        let mac_tag = &raw_req[..mac_size];
        let message_bytes = &raw_req[mac_size..];

        if !recompute_keyed_hmac_and_verify_tag::<GatewayIntegrityHmacAlgorithm>(
            shared_keys.mac_key(),
            message_bytes,
            mac_tag,
        ) {
            return Err(GatewayRequestsError::InvalidMAC);
        }

        let zero_iv = stream_cipher::zero_iv::<GatewayEncryptionAlgorithm>();
        let plaintext = stream_cipher::decrypt::<GatewayEncryptionAlgorithm>(
            shared_keys.encryption_key(),
            &zero_iv,
            &message_bytes,
        );

        Ok(BinaryResponse::PushedMixMessage(plaintext))
    }

    pub fn into_encrypted_tagged_bytes(self, shared_key: &SharedKeys) -> Vec<u8> {
        match self {
            // TODO: it could be theoretically slightly more efficient if the data wasn't taken
            // by reference because then it makes a copy for encryption rather than do it in place
            BinaryResponse::PushedMixMessage(message) => shared_key.encrypt_and_tag(&message, None),
        }
    }

    pub fn new_pushed_mix_message(msg: Vec<u8>) -> Self {
        BinaryResponse::PushedMixMessage(msg)
    }

    pub fn into_ws_message(self, shared_key: &SharedKeys) -> Message {
        Message::Binary(self.into_encrypted_tagged_bytes(shared_key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_payload_can_be_deserialized_into_register_handshake_init_request() {
        let handshake_data = vec![1, 2, 3, 4, 5, 6];
        let handshake_payload = RegistrationHandshake::HandshakePayload {
            data: handshake_data.clone(),
        };
        let serialized = serde_json::to_string(&handshake_payload).unwrap();
        let deserialized = ClientControlRequest::try_from(serialized).unwrap();

        match deserialized {
            ClientControlRequest::RegisterHandshakeInitRequest { data } => {
                assert_eq!(data, handshake_data)
            }
            _ => unreachable!("this branch shouldn't have been reached!"),
        }
    }
}
