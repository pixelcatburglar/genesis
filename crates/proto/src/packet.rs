//! `macho.MachoPacket` and `macho.MachoAddress` (machoNet.py:3303-3820).

use crate::Error;
use marshal::Value;

/// `MACHONETMSG_TYPE_*`. Odd commands are responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    AuthenticationReq = 0,
    AuthenticationRsp = 1,
    IdentificationReq = 2,
    IdentificationRsp = 3,
    CallReq = 6,
    CallRsp = 7,
    TransportClosed = 8,
    ResolveReq = 10,
    ResolveRsp = 11,
    Notification = 12,
    ErrorResponse = 15,
    SessionChangeNotification = 16,
    SessionInitialStateNotification = 18,
    PingReq = 20,
    PingRsp = 21,
}

impl Command {
    pub fn from_i64(v: i64) -> Option<Command> {
        use Command::*;
        Some(match v {
            0 => AuthenticationReq,
            1 => AuthenticationRsp,
            2 => IdentificationReq,
            3 => IdentificationRsp,
            6 => CallReq,
            7 => CallRsp,
            8 => TransportClosed,
            10 => ResolveReq,
            11 => ResolveRsp,
            12 => Notification,
            15 => ErrorResponse,
            16 => SessionChangeNotification,
            18 => SessionInitialStateNotification,
            20 => PingReq,
            21 => PingRsp,
            _ => return None,
        })
    }

    /// The packet class, sent as the instance guid.
    pub fn guid(self) -> &'static str {
        use Command::*;
        match self {
            AuthenticationReq => "macho.AuthenticationReq",
            AuthenticationRsp => "macho.AuthenticationRsp",
            IdentificationReq => "macho.IdentificationReq",
            IdentificationRsp => "macho.IdentificationRsp",
            CallReq => "macho.CallReq",
            CallRsp => "macho.CallRsp",
            TransportClosed => "macho.TransportClosed",
            ResolveReq => "macho.ResolveReq",
            ResolveRsp => "macho.ResolveRsp",
            Notification => "macho.Notification",
            ErrorResponse => "macho.ErrorResponse",
            SessionChangeNotification => "macho.SessionChangeNotification",
            SessionInitialStateNotification => "macho.SessionInitialStateNotification",
            PingReq => "macho.PingReq",
            PingRsp => "macho.PingRsp",
        }
    }

    /// `__machodesc__['params']`; a trailing '?' marks the optional last param.
    pub fn params(self) -> &'static [&'static str] {
        use Command::*;
        match self {
            AuthenticationReq => &["clientinfo", "userName", "password", "address?"],
            AuthenticationRsp => &["serverinfo", "userID", "role", "payload", "proxyNodeID?"],
            IdentificationReq => &["nodeID", "myaddress", "others"],
            IdentificationRsp => &["accepted", "nodeID", "others"],
            CallReq => &["payload", "channel?"],
            CallRsp => &["payload"],
            TransportClosed => &["clientID", "isRemote"],
            ResolveReq => &[],
            ResolveRsp => &["success", "reason"],
            Notification => &["payload", "channel?"],
            ErrorResponse => &["originalCommand", "code", "payload"],
            SessionChangeNotification => &["change", "nodesOfInterest?"],
            SessionInitialStateNotification => &["initialstate"],
            PingReq | PingRsp => &["times"],
        }
    }

    /// `__machodesc__['response']`.
    pub fn response(self) -> Option<Command> {
        use Command::*;
        match self {
            AuthenticationReq => Some(AuthenticationRsp),
            IdentificationReq => Some(IdentificationRsp),
            CallReq => Some(CallRsp),
            ResolveReq => Some(ResolveRsp),
            PingReq => Some(PingRsp),
            _ => None,
        }
    }
}

/// `MACHONETERR_*`, the ErrorResponse codes.
pub mod error_code {
    pub const UNMACHO_DESTINATION: i32 = 0;
    pub const UNMACHO_CHANNEL: i32 = 1;
    pub const WRAPPED_EXCEPTION: i32 = 2;
}

/// `macho.MachoAddress`, carried as its `__getstate__` tuple.
#[derive(Debug, Clone, PartialEq)]
pub enum Address {
    /// ('C', clientID, callID, service)
    Client {
        client_id: Value,
        call_id: Value,
        service: Value,
    },
    /// ('B', broadcastID, narrowcast, idtype)
    Broadcast {
        broadcast_id: Value,
        narrowcast: Value,
        id_type: Value,
    },
    /// ('A', service, callID)
    Any { service: Value, call_id: Value },
    /// ('N', nodeID, service, callID)
    Node {
        node_id: Value,
        service: Value,
        call_id: Value,
    },
}

impl Address {
    pub const GUID: &'static str = "macho.MachoAddress";

    pub fn any() -> Address {
        Address::Any {
            service: Value::None,
            call_id: Value::None,
        }
    }

    pub fn node(node_id: i64) -> Address {
        Address::Node {
            node_id: node_id.into(),
            service: Value::None,
            call_id: Value::None,
        }
    }

    pub fn client(client_id: i64) -> Address {
        Address::Client {
            client_id: client_id.into(),
            call_id: Value::None,
            service: Value::None,
        }
    }

    pub fn broadcast(broadcast_id: &str, narrowcast: Value, id_type: &str) -> Address {
        Address::Broadcast {
            broadcast_id: Value::str(broadcast_id),
            narrowcast,
            id_type: Value::str(id_type),
        }
    }

    pub fn service(&self) -> Option<&str> {
        match self {
            Address::Client { service, .. }
            | Address::Any { service, .. }
            | Address::Node { service, .. } => service.as_str(),
            Address::Broadcast { .. } => None,
        }
    }

    pub fn call_id(&self) -> Option<&Value> {
        match self {
            Address::Client { call_id, .. }
            | Address::Any { call_id, .. }
            | Address::Node { call_id, .. } => Some(call_id),
            Address::Broadcast { .. } => None,
        }
    }

    pub fn to_value(&self) -> Value {
        let state = match self {
            Address::Client {
                client_id,
                call_id,
                service,
            } => Value::tuple([
                Value::str("C"),
                client_id.clone(),
                call_id.clone(),
                service.clone(),
            ]),
            Address::Broadcast {
                broadcast_id,
                narrowcast,
                id_type,
            } => Value::tuple([
                Value::str("B"),
                broadcast_id.clone(),
                narrowcast.clone(),
                id_type.clone(),
            ]),
            Address::Any { service, call_id } => {
                Value::tuple([Value::str("A"), service.clone(), call_id.clone()])
            }
            Address::Node {
                node_id,
                service,
                call_id,
            } => Value::tuple([
                Value::str("N"),
                node_id.clone(),
                service.clone(),
                call_id.clone(),
            ]),
        };
        Value::instance(Self::GUID, state)
    }

    /// `__setstate__`: shorter tuples leave the remaining fields unset (None).
    pub fn from_value(v: &Value) -> Result<Address, Error> {
        let state = match v {
            Value::Instance { state, .. } => state.as_ref(),
            other => other,
        };
        let t = state
            .as_seq()
            .ok_or(Error::Malformed("address state is not a tuple"))?;
        let at = |i: usize| t.get(i).cloned().unwrap_or(Value::None);
        let kind = t
            .first()
            .and_then(|k| k.as_bytes())
            .ok_or(Error::Malformed("address type"))?;
        Ok(match kind {
            b"C" => Address::Client {
                client_id: at(1),
                call_id: at(2),
                service: at(3),
            },
            b"B" => Address::Broadcast {
                broadcast_id: at(1),
                narrowcast: at(2),
                id_type: at(3),
            },
            b"A" => Address::Any {
                service: at(1),
                call_id: at(2),
            },
            _ => Address::Node {
                node_id: at(1),
                service: at(2),
                call_id: at(3),
            },
        })
    }
}

/// A decoded machoNet packet.
#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    pub command: Command,
    pub source: Address,
    pub destination: Address,
    pub user_id: Value,
    /// Positional body, aligned with `command.params()`.
    pub body: Vec<Value>,
    /// Out-of-band dict (`channel`, `compressedPart`, `OID+`, `provisional`, ...).
    pub oob: Vec<(Value, Value)>,
}

impl Packet {
    pub fn new(
        command: Command,
        source: Address,
        destination: Address,
        body: Vec<Value>,
    ) -> Packet {
        Packet {
            command,
            source,
            destination,
            user_id: Value::None,
            body,
            oob: Vec::new(),
        }
    }

    pub fn param(&self, name: &str) -> Option<&Value> {
        let i = self
            .command
            .params()
            .iter()
            .position(|p| p.trim_end_matches('?') == name)?;
        self.body.get(i)
    }

    pub fn oob_get(&self, key: &str) -> Option<&Value> {
        self.oob
            .iter()
            .find(|(k, _)| k.as_bytes() == Some(key.as_bytes()))
            .map(|(_, v)| v)
    }

    pub fn oob_set(&mut self, key: &str, value: Value) {
        self.oob
            .retain(|(k, _)| k.as_bytes() != Some(key.as_bytes()));
        self.oob.push((Value::str(key), value));
    }

    /// `MachoPacket.Response`: swaps addresses and carries userID and channel over.
    pub fn response(&self, body: Vec<Value>) -> Packet {
        let command = self.command.response().unwrap_or(Command::CallRsp);
        let mut rsp = Packet::new(command, self.destination.clone(), self.source.clone(), body);
        rsp.user_id = self.user_id.clone();
        if let Some(ch) = self.oob_get("channel") {
            rsp.oob_set("channel", ch.clone());
        }
        rsp
    }

    /// `MachoPacket.ErrorResponse`.
    pub fn error_response(&self, code: i32, payload: Value) -> Packet {
        let mut rsp = Packet::new(
            Command::ErrorResponse,
            self.destination.clone(),
            self.source.clone(),
            vec![Value::Int(self.command as i32), Value::Int(code), payload],
        );
        rsp.user_id = self.user_id.clone();
        if let Some(ch) = self.oob_get("channel") {
            rsp.oob_set("channel", ch.clone());
        }
        rsp
    }

    /// `__getstate__`: (command, source, destination, userID, tuple(body), oob or None).
    pub fn to_value(&self) -> Value {
        let oob = if self.oob.is_empty() {
            Value::None
        } else {
            Value::Dict(self.oob.clone())
        };
        Value::instance(
            self.command.guid(),
            Value::tuple([
                Value::Int(self.command as i32),
                self.source.to_value(),
                self.destination.to_value(),
                self.user_id.clone(),
                Value::Tuple(self.body.clone()),
                oob,
            ]),
        )
    }

    pub fn from_value(v: &Value) -> Result<Packet, Error> {
        let Value::Instance { state, .. } = v else {
            return Err(Error::Malformed("packet is not an instance"));
        };
        let t = state
            .as_seq()
            .ok_or(Error::Malformed("packet state is not a tuple"))?;
        if t.len() != 6 {
            return Err(Error::Malformed("packet state must have 6 fields"));
        }
        let command = t[0]
            .as_int()
            .and_then(Command::from_i64)
            .ok_or(Error::Malformed("unknown command"))?;
        let body = t[4]
            .as_seq()
            .ok_or(Error::Malformed("packet body is not a tuple"))?
            .to_vec();
        let oob = match &t[5] {
            Value::Dict(pairs) => pairs.clone(),
            _ => Vec::new(),
        };
        Ok(Packet {
            command,
            source: Address::from_value(&t[1])?,
            destination: Address::from_value(&t[2])?,
            user_id: t[3].clone(),
            body,
            oob,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        crate::dumps(&self.to_value())
    }

    pub fn decode(frame: &[u8]) -> Result<Packet, Error> {
        Packet::from_value(&crate::loads(frame)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_roundtrips_through_marshal() {
        let mut p = Packet::new(
            Command::CallReq,
            Address::Client {
                client_id: Value::Int(0),
                call_id: Value::Int(7),
                service: Value::None,
            },
            Address::Any {
                service: Value::str("charMgr"),
                call_id: Value::None,
            },
            vec![Value::tuple([
                Value::Int(0),
                Value::Str(vec![0x7e, 0, 0, 0, 0, 1]),
            ])],
        );
        p.user_id = Value::Int(1);
        p.oob_set("channel", Value::str("x"));
        assert_eq!(Packet::decode(&p.encode()).unwrap(), p);
    }

    #[test]
    fn response_swaps_addresses_and_keeps_channel() {
        let mut req = Packet::new(
            Command::AuthenticationReq,
            Address::client(0),
            Address::any(),
            vec![],
        );
        req.oob_set("channel", Value::str("authentication"));
        let rsp = req.response(vec![]);
        assert_eq!(rsp.command, Command::AuthenticationRsp);
        assert_eq!(rsp.destination, req.source);
        assert_eq!(rsp.oob_get("channel"), Some(&Value::str("authentication")));
    }
}
