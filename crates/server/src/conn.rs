//! One client connection: version check, authentication, then call dispatch.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures::{SinkExt, StreamExt};
use marshal::Value;
use proto::packet::error_code;
use proto::{gpcs, handshake, Address, Command, FrameCodec, Packet};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use crate::registry::Push;
use crate::services::{self, CallError, Session};
use crate::Server;

type Wire = Framed<TcpStream, FrameCodec>;

async fn send(wire: &mut Wire, packet: &Packet) -> Result<()> {
    wire.send(packet.encode()).await?;
    Ok(())
}

async fn flush(wire: &mut Wire, session: &mut Session) -> Result<()> {
    for p in std::mem::take(&mut session.outbox) {
        send(wire, &p).await?;
    }
    Ok(())
}

async fn recv_value(wire: &mut Wire) -> Result<Option<Value>> {
    match wire.next().await {
        Some(frame) => Ok(Some(proto::loads(&frame?)?)),
        None => Ok(None),
    }
}

pub async fn serve(
    server: Arc<Server>,
    stream: TcpStream,
    peer: SocketAddr,
    client_id: i64,
) -> Result<()> {
    let mut wire = Framed::new(stream, FrameCodec);

    // We are the listening side: pitch first, then read the echo
    // (LowLevelVersionCheck with llvercheck=0).
    wire.send(proto::dumps(&handshake::challenge(server.user_count())))
        .await?;
    let Some(reply) = recv_value(&mut wire).await? else {
        tracing::debug!(%peer, "closed during version check");
        return Ok(());
    };
    if let Err(why) = handshake::check(&reply) {
        // The client turns a bare string into GPSTransportClosed(reason).
        wire.send(proto::dumps(&Value::str(&why))).await.ok();
        bail!("version check failed: {why}");
    }
    tracing::info!(%peer, "version check ok: {reply:?}");

    let mut session = Session::new(client_id);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Push>();
    server.registry.insert(client_id, tx);
    let result = run(&server, &mut wire, &mut session, &mut rx, peer).await;
    services::gang::on_disconnect(&server, &mut session);
    services::space::on_disconnect(&server, &session).await;
    if let Some(char_id) = session.char_id() {
        services::trade::on_disconnect(&server, char_id).await;
    }
    server.registry.remove(client_id);
    tracing::info!(%peer, user = ?session.user_id, "disconnected");
    result
}

async fn run(
    server: &Arc<Server>,
    wire: &mut Wire,
    session: &mut Session,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Push>,
    peer: SocketAddr,
) -> Result<()> {
    let server = server.clone();
    loop {
        // Packets pushed by other connections (chat, space) between calls.
        let v = tokio::select! {
            v = recv_value(wire) => match v? { Some(v) => v, None => return Ok(()) },
            Some(push) = rx.recv() => {
                match push {
                    Push::Packet(p) => send(wire, &p).await?,
                    Push::Change(changes) => {
                        session.change(&server, &changes);
                        flush(wire, session).await?;
                    }
                    Push::BindEvent { service, params, event, mut args } => {
                        args.push(session.bind_value(&server, &service, params));
                        session.event(&server, &event, args);
                        flush(wire, session).await?;
                    }
                    Push::Destroyed { system, ship } => {
                        if let Err(e) = services::space::destroy_ship(&server, session, system, ship).await {
                            tracing::warn!(%peer, "ship destruction failed: {e:#}");
                        }
                        flush(wire, session).await?;
                        for p in std::mem::take(&mut session.after) {
                            send(wire, &p).await?;
                        }
                    }
                }
                continue;
            }
        };
        let packet = match Packet::from_value(&v) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(%peer, "undecodable packet ({e}): {v:?}");
                continue;
            }
        };
        match packet.command {
            Command::AuthenticationReq => {
                let rsp = authenticate(&server, session, &packet).await?;
                send(wire, &rsp).await?;
                flush(wire, session).await?;
            }
            _ if session.user_id.is_none() => {
                // machoNet closes unauthenticated wires that send anything else.
                bail!("{:?} before authentication", packet.command);
            }
            Command::CallReq => {
                let rsp = call(&server, session, &packet).await;
                // Session changes and events raised by the call go first, as
                // the original server's SessionChanged did before returning.
                flush(wire, session).await?;
                send(wire, &rsp).await?;
                for p in std::mem::take(&mut session.after) {
                    send(wire, &p).await?;
                }
            }
            Command::PingReq => {
                let mut times = packet
                    .param("times")
                    .and_then(Value::as_seq)
                    .map(<[Value]>::to_vec)
                    .unwrap_or_default();
                let now = crate::statics::blue_time_now();
                times.push(Value::tuple([
                    Value::Long(now),
                    Value::Long(now),
                    Value::str("server::turnaround"),
                ]));
                send(wire, &packet.response(vec![Value::List(times)])).await?;
            }
            Command::Notification => {
                tracing::info!(%peer, "client notification: {:?}", packet.body);
            }
            // Answers to client_call (comtool.ChatInvite and the like).
            Command::CallRsp | Command::ErrorResponse => {
                tracing::debug!(%peer, "client answered a server call: {:?}", packet.body);
            }
            other => tracing::warn!(%peer, "unhandled {other:?}: {packet:?}"),
        }
    }
}

async fn authenticate(server: &Server, session: &mut Session, req: &Packet) -> Result<Packet> {
    let user = req
        .param("userName")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let password = req
        .param("password")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    tracing::info!(user, clientinfo = ?req.param("clientinfo"), "authentication request");

    let account = crate::accounts::login(
        &server.pool,
        &user,
        &password,
        server.config.auto_create_accounts,
    )
    .await?;
    let Some((user_id, role)) = account else {
        let err = gpcs::user_error("AuthenticationFailure", Value::Dict(Vec::new()));
        return Ok(req.error_response(error_code::WRAPPED_EXCEPTION, Value::tuple([err])));
    };
    session.user_id = Some(user_id);
    session.role = role;
    session.user_name = user;
    // The client's session learns its user and role from the server.
    session.change(
        server,
        &[
            ("userid", Value::from(user_id)),
            ("role", Value::from(role)),
        ],
    );

    // (initvals, serviceInfo). An empty serviceInfo routes every service call to
    // an 'any' address, which this single node answers.
    let payload = Value::tuple([
        server.statics.initvals(server.config.node_id),
        Value::Dict(Vec::new()),
    ]);
    let mut rsp = req.response(vec![
        Value::dict([
            (Value::str("version"), Value::Float(10.0)),
            (Value::str("build"), Value::Int(1000)),
            (Value::str("codename"), Value::str("Gemini")),
        ]),
        Value::from(user_id),
        Value::from(role),
        gpcs::auth_payload(&payload),
        Value::from(server.config.node_id),
    ]);
    rsp.user_id = Value::from(user_id);
    Ok(rsp)
}

async fn call(server: &Server, session: &mut Session, req: &Packet) -> Packet {
    let parsed = req
        .param("payload")
        .context("CallReq without payload")
        .and_then(|p| gpcs::parse_call(p).map_err(Into::into));
    let call = match parsed {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("bad CallReq ({e:#}): {:?}", req.body);
            let err = gpcs::user_error(
                "UnMachoDestination",
                Value::dict([(Value::str("what"), Value::str(e.to_string()))]),
            );
            return req.error_response(error_code::WRAPPED_EXCEPTION, Value::tuple([err]));
        }
    };
    let service = req.destination.service().unwrap_or_default().to_owned();
    match services::dispatch(server, session, &service, &call).await {
        Ok(retval) => req.response(vec![gpcs::call_response(&retval)]),
        Err(CallError::User { msg, dict }) => {
            tracing::info!(service, method = call.method(), "UserError {msg}");
            req.error_response(
                error_code::WRAPPED_EXCEPTION,
                Value::tuple([gpcs::user_error(&msg, dict)]),
            )
        }
        Err(CallError::Internal(e)) => {
            tracing::error!(service, method = call.method(), "call failed: {e:#}");
            let err = gpcs::user_error(
                "UnMachoDestination",
                Value::dict([(Value::str("what"), Value::str(format!("{e:#}")))]),
            );
            req.error_response(error_code::WRAPPED_EXCEPTION, Value::tuple([err]))
        }
    }
}

/// A client event: BroadcastStuff dispatches it as sm.ScatterEvent(name, *args)
/// (ChainEvent for Process*, SendEvent for Do*).
pub fn event(node_id: i64, client_id: i64, name: &str, args: Vec<Value>) -> Packet {
    Packet::new(
        Command::Notification,
        Address::node(node_id),
        Address::broadcast(name, Value::List(vec![Value::from(client_id)]), "clientID"),
        vec![gpcs::event_payload(args)],
    )
}

static CLIENT_CALL_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

/// A call on a client's service (the server side of exported calls such as
/// comtool.ChatInvite). The payload is what ObjectCall/ServiceCall unwrap on
/// the way up: (0, marshal((1, method, args, {}))). The client answers with a
/// CallRsp we do not wait for.
pub fn client_call(
    node_id: i64,
    client_id: i64,
    service: &str,
    method: &str,
    args: Vec<Value>,
) -> Packet {
    let call_id = CLIENT_CALL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let inner = Value::tuple([
        Value::Int(1),
        Value::str(method),
        Value::Tuple(args),
        Value::Dict(Vec::new()),
    ]);
    let payload = Value::tuple([Value::Int(0), Value::Str(marshal::encode(&inner))]);
    Packet::new(
        Command::CallReq,
        Address::Node {
            node_id: Value::from(node_id),
            service: Value::None,
            call_id: Value::from(call_id),
        },
        Address::Client {
            client_id: Value::from(client_id),
            call_id: Value::None,
            service: Value::str(service),
        },
        vec![payload],
    )
}
