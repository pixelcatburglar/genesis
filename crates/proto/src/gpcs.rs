//! GPCS payload wrapping.
//!
//! Each machoNet channel runs the payload through a stack of handlers
//! (machoNet.py gpcsMap). The client's default stack, wire side first:
//! ExceptionWrapper, ExceptionMapping, ObjectCall, ServiceCall, BroadcastStuff.
//! The authentication channel stops at ObjectCall.

use crate::Error;
use marshal::Value;

/// A decoded CallReq payload.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// ServiceCall.RemoteServiceCall: inner `(1, method, args, kwargs)`; the
    /// service name is in the destination address.
    Service {
        method: String,
        args: Vec<Value>,
        kwargs: Value,
    },
    /// ObjectCall.ObjectCallWithoutTheStars: inner `(objectID, method, args, kwargs)`.
    Object {
        object_id: String,
        method: String,
        args: Vec<Value>,
        kwargs: Value,
    },
}

impl Call {
    pub fn method(&self) -> &str {
        match self {
            Call::Service { method, .. } | Call::Object { method, .. } => method,
        }
    }

    pub fn args(&self) -> &[Value] {
        match self {
            Call::Service { args, .. } | Call::Object { args, .. } => args,
        }
    }

    pub fn kwargs(&self) -> &Value {
        match self {
            Call::Service { kwargs, .. } | Call::Object { kwargs, .. } => kwargs,
        }
    }
}

/// Unwraps `CallReq.payload` = `(flag, marshal(inner))` (ObjectCall.CallDown /
/// ObjectCallWithoutTheStars): flag 0 is a service call, 1 an object call.
pub fn parse_call(payload: &Value) -> Result<Call, Error> {
    let outer = payload
        .as_seq()
        .ok_or(Error::Malformed("call payload is not a tuple"))?;
    let flag = outer
        .first()
        .and_then(Value::as_int)
        .ok_or(Error::Malformed("call flag"))?;
    let pickle = outer
        .get(1)
        .and_then(Value::as_bytes)
        .ok_or(Error::Malformed("inner pickle"))?;
    let inner = marshal::decode(pickle)?;
    let t = inner
        .as_seq()
        .ok_or(Error::Malformed("inner call is not a tuple"))?;
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_owned);
    let args = |v: Option<&Value>| {
        v.and_then(Value::as_seq)
            .map(<[Value]>::to_vec)
            .unwrap_or_default()
    };
    let kwargs = |v: Option<&Value>| v.cloned().unwrap_or(Value::Dict(Vec::new()));
    if flag == 0 {
        // ServiceCall.CallUp: payload[0] true means a direct call.
        if t.first().and_then(Value::as_int) != Some(1) {
            return Err(Error::Malformed(
                "service call is not (1, method, args, kwargs)",
            ));
        }
        Ok(Call::Service {
            method: text(t.get(1)).ok_or(Error::Malformed("method name"))?,
            args: args(t.get(2)),
            kwargs: kwargs(t.get(3)),
        })
    } else {
        Ok(Call::Object {
            object_id: text(t.first()).ok_or(Error::Malformed("object id"))?,
            method: text(t.get(1)).ok_or(Error::Malformed("method name"))?,
            args: args(t.get(2)),
            kwargs: kwargs(t.get(3)),
        })
    }
}

/// `CallRsp.payload`: ObjectCall.CallUp pickles the return value (a bare stream).
pub fn call_response(retval: &Value) -> Value {
    Value::Str(marshal::encode(retval))
}

/// `AuthenticationRsp.payload`: also a bare stream (ObjectCall.CallUp, AUTH branch).
pub fn auth_payload(payload: &Value) -> Value {
    Value::Str(marshal::encode(payload))
}

/// Payload of an event Notification to a client. BroadcastStuff builds
/// `(1, args, seq)`; ServiceCall, ObjectCall (with the inner pickle) and
/// ExceptionWrapper each wrap it as `(0, ...)`. seq 0 skips WaitForSequenceNumber.
pub fn event_payload(args: Vec<Value>) -> Value {
    let broadcast = Value::tuple([Value::Int(1), Value::Tuple(args), Value::Int(0)]);
    let service = Value::tuple([Value::Int(0), broadcast]);
    let object = Value::tuple([Value::Int(0), Value::Str(marshal::encode(&service))]);
    Value::tuple([Value::Int(0), object])
}

/// A remote bound-object reference, `N=<node>:<oid>`. The client's unmarshal
/// callback (ObjectCall.ParseObjectID) turns tag 0x19 strings into
/// MachoObjectConnection proxies.
pub fn object_ref(node_id: i64, oid: i64) -> Value {
    Value::Custom(Box::new(Value::str(format!("N={node_id}:{oid}"))))
}

/// A UserError raised to the client. `exceptions.UserError` cannot be used: the
/// client imports `exceptions`, which is Python's builtin module and has no
/// UserError. `__builtin__.UserError` resolves; its state is splatted onto
/// `__dict__` (no __setstate__), and the client reads msg, dict AND args.
pub fn user_error(msg: &str, dict: Value) -> Value {
    Value::instance(
        "__builtin__.UserError",
        Value::dict([
            (Value::str("msg"), Value::str(msg)),
            (Value::str("dict"), dict.clone()),
            (Value::str("args"), Value::tuple([Value::str(msg), dict])),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_and_object_calls() {
        let inner = Value::tuple([
            Value::Int(1),
            Value::str("GetCharactersToSelect"),
            Value::tuple([]),
            Value::dict([(Value::str("machoVersion"), Value::Int(1))]),
        ]);
        let p = Value::tuple([Value::Int(0), Value::Str(marshal::encode(&inner))]);
        let c = parse_call(&p).unwrap();
        assert_eq!(c.method(), "GetCharactersToSelect");
        assert!(matches!(c, Call::Service { .. }));

        let inner = Value::tuple([
            Value::str("N=1:5"),
            Value::str("List"),
            Value::tuple([Value::Int(3)]),
        ]);
        let p = Value::tuple([Value::Int(1), Value::Str(marshal::encode(&inner))]);
        match parse_call(&p).unwrap() {
            Call::Object {
                object_id, args, ..
            } => {
                assert_eq!(object_id, "N=1:5");
                assert_eq!(args, vec![Value::Int(3)]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn event_payload_unwraps_like_the_client() {
        let p = event_payload(vec![Value::Int(9)]);
        let ew = &p.as_seq().unwrap()[1];
        let pickled = ew.as_seq().unwrap()[1].as_bytes().unwrap();
        let sc = marshal::decode(pickled).unwrap();
        let bs = &sc.as_seq().unwrap()[1];
        assert_eq!(
            bs,
            &Value::tuple([Value::Int(1), Value::tuple([Value::Int(9)]), Value::Int(0)])
        );
    }
}
