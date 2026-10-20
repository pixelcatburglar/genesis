//! Remote services, dispatched by (service, method), and bound objects.
//!
//! The client's full remote surface is listed in docs/client-rpc.json
//! (tools/harvest_rpc.py) and docs/client-bound.json (tools/harvest_bound.py).
//! Unimplemented calls answer None and are logged:
//! the client's ChainEvent swallows per-handler errors but an UNANSWERED call
//! stalls the whole chain, so answering is always better than silence.
//!
//! Bound objects (util/moniker.py): the client first calls
//! `<service>.MachoResolveObject(bindParams, justQuery)` for a node ID, then
//! `<service>.MachoBindObject(bindParams, call)`, which returns
//! `(objectRef, result of the piggybacked call)` (ServiceCall.CallUp). Later
//! calls arrive as object calls on `N=<node>:<oid>`.

mod agents;
mod bookmark;
mod broker;
pub mod character;
pub mod chat;
pub mod combat;
mod config;
pub mod corp;
mod dogma;
pub mod factory;
pub mod gang;
pub mod inventory;
mod mail;
mod market;
mod missions;
mod news;
mod object_caching;
mod petition;
mod repair;
mod reprocessing;
pub mod skills;
mod slash;
pub mod space;
mod standing;
pub mod trade;
mod station;
pub mod wallet;

use std::collections::HashMap;

use marshal::Value;
use proto::gpcs::{self, Call};
use proto::Packet;

use crate::session::{self, Attributes};
use crate::Server;

pub struct Session {
    pub client_id: i64,
    pub user_id: Option<i64>,
    pub user_name: String,
    pub role: i64,
    /// What the client's session holds, as last told by us.
    pub attrs: Attributes,
    /// Packets to send ahead of the current call's response.
    pub outbox: Vec<Packet>,
    /// Packets to send right after it (e.g. beyonce's SetState, which needs
    /// the client to have installed the park that the bind returns).
    pub after: Vec<Packet>,
    pub bound: HashMap<i64, Bound>,
    next_oid: i64,
}

/// A bound object: the service it belongs to and its bind parameters.
#[derive(Debug, Clone)]
pub struct Bound {
    pub service: String,
    pub params: Value,
}

impl Session {
    pub fn new(client_id: i64) -> Session {
        Session {
            client_id,
            user_id: None,
            user_name: String::new(),
            role: 0,
            attrs: Attributes::default(),
            outbox: Vec::new(),
            after: Vec::new(),
            bound: HashMap::new(),
            next_oid: 1,
        }
    }

    pub fn char_id(&self) -> Option<i32> {
        self.attrs.int("charid").map(|c| c as i32)
    }

    /// Updates our copy of the session and queues the notification.
    pub fn change(&mut self, server: &Server, changes: &[(&'static str, Value)]) {
        // sessions.py: locrole is the character item's flag. Characters are
        // always flagPilot here (inventory::character_item); gameui.OnNewState
        // closes the ship UI for anyone whose locrole is not flagPilot.
        let mut changes = changes.to_vec();
        if let Some((_, ch)) = changes.iter().find(|(k, _)| *k == "charid") {
            let locrole = if ch.is_none() { Value::None } else { Value::Int(server.world.flag("Pilot")) };
            changes.push(("locrole", locrole));
        }
        let diff = self.attrs.apply(&changes);
        if let Some(p) = session::change_notification(server.config.node_id, self.client_id, &diff)
        {
            tracing::info!(client = self.client_id, change = ?diff, "session change");
            self.outbox.push(p);
        }
        let role = self.role;
        let gang = self.attrs.int("gangid");
        let (char_id, corp_id, location_id, ss) = (
            self.char_id(),
            self.attrs.int("corpid").map(|c| c as i32),
            self.attrs.int("locationid"),
            self.attrs.int("solarsystemid"),
        );
        server.registry.update(self.client_id, |c| {
            c.char_id = char_id;
            c.corp_id = corp_id;
            c.location_id = location_id;
            c.solar_system_id = ss;
            c.role = role;
            c.gang_id = gang;
        });
    }

    /// Binds a server-side object and returns the reference the client
    /// unmarshals into a remote object proxy.
    pub fn bind_value(&mut self, server: &Server, service: &str, params: Value) -> Value {
        let oid = self.bind(service, params);
        tracing::debug!(service, oid, "bound sub-object");
        gpcs::object_ref(server.config.node_id, oid)
    }

    /// Queues a client event (sm.ScatterEvent on the client).
    pub fn event(&mut self, server: &Server, name: &str, args: Vec<Value>) {
        self.outbox.push(crate::conn::event(
            server.config.node_id,
            self.client_id,
            name,
            args,
        ));
    }

    fn bind(&mut self, service: &str, params: Value) -> i64 {
        let oid = self.next_oid;
        self.next_oid += 1;
        self.bound.insert(
            oid,
            Bound {
                service: service.to_owned(),
                params,
            },
        );
        oid
    }
}

#[derive(Debug)]
pub enum CallError {
    /// Raised client-side as UserError(msg, dict). The key must exist in the
    /// messages table or res:/messages.txt, or the client's handler itself fails.
    User {
        msg: String,
        dict: Value,
    },
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for CallError {
    fn from(e: anyhow::Error) -> Self {
        CallError::Internal(e)
    }
}

impl From<tokio_postgres::Error> for CallError {
    fn from(e: tokio_postgres::Error) -> Self {
        CallError::Internal(e.into())
    }
}

impl From<deadpool_postgres::PoolError> for CallError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        CallError::Internal(e.into())
    }
}

pub type CallResult = Result<Value, CallError>;

/// Positional and keyword arguments of a call, read leniently: Python callers
/// pass ints/longs/floats interchangeably and omit trailing defaults.
pub struct Args<'a> {
    pub pos: &'a [Value],
    pub kw: &'a Value,
}

impl<'a> Args<'a> {
    pub fn of(call: &'a Call) -> Args<'a> {
        Args {
            pos: call.args(),
            kw: call.kwargs(),
        }
    }

    /// Positional `i`, else keyword `name`.
    pub fn get(&self, i: usize, name: &str) -> Option<&'a Value> {
        self.pos
            .get(i)
            .or_else(|| self.kw.get(name))
            .filter(|v| !v.is_none())
    }

    pub fn int(&self, i: usize) -> Option<i64> {
        self.pos.get(i).and_then(Value::as_int)
    }

    pub fn i32(&self, i: usize) -> Option<i32> {
        self.int(i).map(|v| v as i32)
    }

    pub fn float(&self, i: usize) -> Option<f64> {
        self.pos.get(i).and_then(Value::as_float)
    }

    pub fn text(&self, i: usize) -> Option<String> {
        self.pos.get(i).and_then(Value::as_text)
    }

    pub fn ints(&self, i: usize) -> Vec<i64> {
        match self.pos.get(i) {
            Some(v) => match v.as_seq() {
                Some(s) => s.iter().filter_map(Value::as_int).collect(),
                None => v.as_int().into_iter().collect(),
            },
            None => Vec::new(),
        }
    }
}

/// Sends a client event (sm.ScatterEvent) to every connection playing one of
/// `chars`.
pub fn notify_chars(server: &Server, chars: &[i32], name: &str, args: Vec<Value>) {
    for (id, c) in server
        .registry
        .select(None, |c| c.char_id.is_some_and(|ch| chars.contains(&ch)))
    {
        c.send(crate::conn::event(
            server.config.node_id,
            id,
            name,
            args.clone(),
        ));
    }
}

/// Changes a character's session: in place when it is the caller (so the
/// notification precedes the reply), else through its connection's task.
pub fn change_char_session(
    server: &Server,
    session: &mut Session,
    char_id: i32,
    changes: Vec<(&'static str, Value)>,
) {
    if session.char_id() == Some(char_id) {
        session.change(server, &changes);
    } else {
        for (_, c) in server
            .registry
            .select(Some(session.client_id), |c| c.char_id == Some(char_id))
        {
            c.change(changes.clone());
        }
    }
}

/// Is this character connected right now?
pub fn is_online(server: &Server, char_id: i32) -> bool {
    !server
        .registry
        .select(None, |c| c.char_id == Some(char_id))
        .is_empty()
}

/// Escapes LIKE wildcards in text typed by players.
pub fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub fn user_error(msg: &str, dict: Vec<(&str, Value)>) -> CallError {
    CallError::User {
        msg: msg.to_owned(),
        dict: Value::Dict(dict.into_iter().map(|(k, v)| (Value::str(k), v)).collect()),
    }
}

/// A service call (`(service, method)` on an address) or an object call.
pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    service: &str,
    call: &Call,
) -> CallResult {
    match call {
        Call::Service { method, args, .. } => match method.as_str() {
            "MachoResolveObject" => Ok(Value::from(server.config.node_id)),
            "MachoBindObject" => {
                let params = args.first().cloned().unwrap_or(Value::None);
                let oid = session.bind(service, params);
                let reference = gpcs::object_ref(server.config.node_id, oid);
                // The piggybacked first call: (method, args, kwargs) or None.
                let result = match args.get(1).and_then(Value::as_seq) {
                    Some([m, a, ..]) => {
                        let first = Call::Object {
                            object_id: format!("N={}:{}", server.config.node_id, oid),
                            method: m.as_str().unwrap_or_default().to_owned(),
                            args: a.as_seq().map(<[Value]>::to_vec).unwrap_or_default(),
                            kwargs: Value::Dict(Vec::new()),
                        };
                        dispatch_object(server, session, oid, &first).await?
                    }
                    _ => Value::None,
                };
                tracing::info!(service, oid, params = ?session.bound[&oid].params, "bound object");
                if service == "beyonce" {
                    let system = session.bound[&oid].params.as_int().unwrap_or(0) as i32;
                    space::send_state(server, session, system).await?;
                }
                Ok(Value::tuple([reference, result]))
            }
            _ => dispatch_service(server, session, service, method, call).await,
        },
        Call::Object { object_id, .. } => {
            let oid = object_id
                .rsplit(':')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(-1);
            dispatch_object(server, session, oid, call).await
        }
    }
}

async fn dispatch_service(
    server: &Server,
    session: &mut Session,
    service: &str,
    method: &str,
    call: &Call,
) -> CallResult {
    tracing::debug!(service, method, args = ?call.args(), "call");
    match service {
        "objectCaching" if method == "GetCachableObject" => {
            object_caching::get_cachable_object(server, call)
        }
        // sys/eve.py SynchronizeClock: the reply is the server's blue.os.GetTime(),
        // fed straight into blue.pyos.synchro.ResetClock.
        "authentication" if method == "Ping" => Ok(Value::Long(crate::statics::blue_time_now())),
        "character" => character::dispatch(server, session, method, call).await,
        "config" => config::dispatch(server, session, method, call).await,
        "dogmaIM" => match method {
            "GetAttributeTypes" => dogma::attribute_types(server).await,
            // {skillTypeID: level} (infosvc DrawSkillTree, market, ccutil).
            "GetRequiredSkillLevels" | "GetSkillPrerequisites" => {
                Ok(skills::required_skills(server, Args::of(call).i32(0).unwrap_or(0)).await?)
            }
            _ => unimplemented(service, method, call),
        },
        "chatx" => chat::service(server, session, method, call),
        "corpmgr" => corp::dispatch(server, session, method, call).await,
        "account" => wallet::dispatch(server, session, method, call).await,
        "bookmark" => bookmark::dispatch(server, session, method, call).await,
        "voucher" if method == "GetObject" => bookmark::get_object(server, session, call).await,
        "pager" => mail::dispatch(server, session, method, call).await,
        "petitioner" => petition::dispatch(server, session, method, call).await,
        "newsmgr" => news::dispatch(server, session, method, call).await,
        "standingmgr" => standing::standingmgr(server, session, method, call).await,
        "standing" => standing::standing(server, session, method, call).await,
        "gangsta" => gang::dispatch(server, session, method, call).await,
        "newBreedAgents" => agents::dispatch(server, session, method, call).await,
        "missionMgr" => missions::dispatch(server, session, method, call).await,
        "factory" => factory::service(server, session, method, call).await,
        "reprocessing" => reprocessing::dispatch(server, session, method, call).await,
        "repair" => repair::dispatch(server, session, method, call).await,
        "trademgr" => trade::dispatch(server, session, method, call).await,
        "market" | "marketBroker" => market::service(server, session, service, method, call).await,
        "station" | "charmgr" | "shipSvc" | "lookupSvc" | "billMgr" | "slash" | "machoNet" => {
            station::dispatch(server, session, service, method, call).await
        }
        _ => unimplemented(service, method, call),
    }
}

async fn dispatch_object(
    server: &Server,
    session: &mut Session,
    oid: i64,
    call: &Call,
) -> CallResult {
    let Some(bound) = session.bound.get(&oid).cloned() else {
        return Err(user_error(
            "UnMachoDestination",
            vec![("what", Value::str(format!("no bound object {oid}")))],
        ));
    };
    let method = call.method();
    tracing::debug!(service = bound.service, oid, method, args = ?call.args(), "object call");
    match bound.service.as_str() {
        "invbroker" => inventory::broker(server, session, &bound, call).await,
        "inventory" => inventory::inventory(server, session, &bound, call).await,
        "dogmaIM" => match call.method() {
            "Activate" | "Deactivate" | "AddTarget" | "RemoveTarget" | "ClearTargets"
            | "GetTargets" | "GetTargeters" => combat::dogma_call(server, session, call).await,
            _ => dogma::location(server, session, call).await,
        },
        "entity" => combat::entity(server, session, call).await,
        "market" => market::bound(server, session, &bound, call).await,
        "missionMgr" => missions::bound(server, session, &bound, call).await,
        "factory" => factory::bound(server, session, &bound, call).await,
        "trademgr" => trade::bound(server, session, &bound, call).await,
        "broker" => broker::bound(server, session, &bound, call).await,
        "corpStationMgr" => corp::station_mgr(server, session, call).await,
        "skillMgr" => skills::skill_mgr(server, session, call).await,
        "aggressionMgr" => {
            let system = bound.params.as_int().unwrap_or(0) as i32;
            space::aggression(server, session, system, call).await
        }
        "voucher" => bookmark::voucher(server, session, &bound, call).await,
        "chatx" => chat::channel(server, session, &bound, call).await,
        "ship" => space::ship(server, session, call).await,
        "beyonce" => {
            let system = bound.params.as_int().unwrap_or(0) as i32;
            space::beyonce(server, session, system, call).await
        }
        _ => unimplemented(
            &format!("{}[{:?}]", bound.service, bound.params),
            method,
            call,
        ),
    }
}

pub fn unimplemented(service: &str, method: &str, call: &Call) -> CallResult {
    tracing::warn!(service, method, args = ?call.args(), kwargs = ?call.kwargs(), "UNIMPLEMENTED call answered None");
    Ok(Value::None)
}
