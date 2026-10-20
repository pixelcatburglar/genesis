//! Every connected client, for pushing packets to other connections (chat,
//! space updates). Each connection drains its channel in its select loop.

use std::collections::HashMap;
use std::sync::Mutex;

use marshal::Value;
use proto::Packet;
use tokio::sync::mpsc::UnboundedSender;

/// What other tasks push into a connection.
#[derive(Debug)]
pub enum Push {
    /// Sent as is.
    Packet(Packet),
    /// Applied to the connection's session (and the client told), e.g. a gang
    /// kick or a corporation change made by someone else.
    Change(Vec<(&'static str, Value)>),
    /// The connection's ship was destroyed in space by someone else; the
    /// connection runs the destruction itself so its session follows.
    Destroyed { system: i32, ship: i64 },
    /// Binds `service[params]` in the connection's session and sends `event`
    /// with the object reference appended to `args` (a trade offered by
    /// someone else).
    BindEvent { service: String, params: Value, event: String, args: Vec<Value> },
}

#[derive(Debug, Clone)]
pub struct Client {
    pub tx: UnboundedSender<Push>,
    pub char_id: Option<i32>,
    pub char_name: String,
    pub corp_id: Option<i32>,
    pub location_id: Option<i64>,
    pub solar_system_id: Option<i64>,
    /// ROLE_* bits of the account.
    pub role: i64,
    pub gang_id: Option<i64>,
    /// Custom chat channels joined (chatx channelType 0).
    pub channels: Vec<i64>,
}

impl Client {
    pub fn send(&self, p: Packet) {
        self.tx.send(Push::Packet(p)).ok();
    }

    pub fn change(&self, changes: Vec<(&'static str, Value)>) {
        self.tx.send(Push::Change(changes)).ok();
    }
}

#[derive(Default)]
pub struct Registry {
    clients: Mutex<HashMap<i64, Client>>,
}

impl Registry {
    pub fn insert(&self, client_id: i64, tx: UnboundedSender<Push>) {
        let c = Client {
            tx,
            char_id: None,
            char_name: String::new(),
            corp_id: None,
            location_id: None,
            solar_system_id: None,
            role: 0,
            gang_id: None,
            channels: Vec::new(),
        };
        self.clients.lock().unwrap().insert(client_id, c);
    }

    pub fn remove(&self, client_id: i64) -> Option<Client> {
        self.clients.lock().unwrap().remove(&client_id)
    }

    pub fn update(&self, client_id: i64, f: impl FnOnce(&mut Client)) {
        if let Some(c) = self.clients.lock().unwrap().get_mut(&client_id) {
            f(c);
        }
    }

    /// Clients matching a predicate, except `except`.
    pub fn select(
        &self,
        except: Option<i64>,
        pred: impl Fn(&Client) -> bool,
    ) -> Vec<(i64, Client)> {
        self.clients
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, c)| Some(**id) != except && pred(c))
            .map(|(id, c)| (*id, c.clone()))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.clients.lock().unwrap().len()
    }
}
