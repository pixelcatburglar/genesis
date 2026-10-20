# Build-1000 wire protocol

Everything below is read off the client itself: Python from
`decompiled/src` (uncompyle6 over `script/compiled.code`), native code from
`decompiled/native` (headless Ghidra). Addresses are blue.dll / NetClient.dll
virtual addresses. Implemented in `crates/marshal` and `crates/proto`.

## 1. Transport

The client connects with `tcp:packet:server` → `gps.NetClientTransportFactory`
→ native `NetClient.AsyncPacketSocket` (net/NetClientGPS.py). Host and port come
from the login screen (defaults `localhost`, `26000`; ui/login/login.py:241).
No patching or redirection is needed.

**Framing** (NetClient.dll 0x10002101–0x10002155): u32 little-endian length,
then the payload. A frame is complete when `buffered - 4 >= length`. Lengths
over **1,000,000** (`cmp esi, 0xF4240` @ 0x1000213D) reset the socket with no
error to the user.

**Compression** (MachoTransport.Read, machoNet.py): if a payload's first byte
is not `~` (0x7E) it is zlib-inflated first. The client compresses its own
packets when that saves >5% on >200 bytes, so the server must accept both.

## 2. blue.marshal

Reader `FUN_10028950`, writer `FUN_10029720`, `Load` `FUN_10029390`, writer
setup `FUN_10028550`, finaliser `FUN_10028630`, share tracking `FUN_100287b0`.

```
u8   0x7E           header (anything else: handed to cPickle.loads)
u32  N              share-map size
...  one object
u32  map[N]         slot (1..N) of each 0x40-flagged object, in stream order
```

Lengths (`FUN_100288d0` / `FUN_10028740`): a u8, or `0xFF` + u32 when ≥ 255.

| tag | meaning | payload |
|---|---|---|
| 01 | None | |
| 02 | builtin type | len + name (`__builtin__.<name>`) |
| 03 | long | i64 |
| 04 / 05 / 06 | int | i32 / i16 / i8 |
| 07 / 08 / 09 | int −1 / 0 / 1 | |
| 0A | float | f64 |
| 0B | float 0.0 | |
| 0C | *unassigned* – "Invalid type tag" | |
| 0D | str ≥ 256 | len + bytes |
| 0E | str "" | |
| 0F | str, 1 char | byte |
| 10 | str < 256 | u8 len + bytes |
| 11 | str from string table | u8 index (1-based) |
| 12 | unicode | len (bytes) + UCS-2 LE |
| 13 | buffer (reads back as str) | len + bytes |
| 14 / 15 | tuple / list | len + items |
| 16 | dict | len + (**value**, key) pairs — value first |
| 17 | instance | guid object (`"module.Class"`) + state object |
| 18 | blue object | len + object image (`BeROT` load) |
| 19 | custom | object passed to the Load callback |
| 1A | pickle | len + cPickle stream (writer fallback for other types) |
| 1B | shared ref | len-encoded slot |
| 1C | checksum | u32 + object |

**Instances**: the writer emits `__getstate__()` if the class has it, else
`__dict__`. The reader creates a raw instance and calls `__setstate__(state)`
if present, else uses state as `__dict__`. A guid resolves by importing the
part before the last dot and taking the attribute.

**String table**: `script:/sys/strings.txt`, 81 entries. Lines are split on
`\n` and the byte before it (the `\r`) is dropped; the text after the last
newline is entry 81 (`util.Rowset`).

**Sharing**: an object the writer meets a second time gets the next slot
number; the `0x40` flag is back-patched onto its first tag byte and the
repeat is `1B slot`. Only 13, 14, 15, 16, 17, 18 can carry the flag.
Containers claim their slot *before* reading children; instances, buffers
and blue objects claim it after.

## 3. Version check (machoNet.py LowLevelVersionCheck)

The listener sends `(170472, machoVersion, userCount, version, build, codename)`;
the connector validates length 6, `[0] == 170472`, `[1] == machoVersion` and
echoes the same shape. `machoVersion = 40` (+1000 packetGuard, +10000
packetCRC32, both off by default). A bare string instead closes the
transport with that string as the reason.

## 4. Packets

A packet is an instance of `macho.<Kind>` with state
`(command, source, destination, userID, body, oob)`; `body` is positional per
`__machodesc__['params']` (a trailing `?` param is optional); `oob` is None or
a dict (`channel`, `compressedPart`, `OID+`, `OID-`, `provisional`, `jit`,
`machoTimeout`).

| cmd | kind | params |
|---|---|---|
| 0 | AuthenticationReq | clientinfo, userName, password, address? |
| 1 | AuthenticationRsp | serverinfo, userID, role, payload, proxyNodeID? |
| 6 | CallReq | payload, channel? |
| 7 | CallRsp | payload |
| 8 | TransportClosed | clientID, isRemote |
| 10 / 11 | ResolveReq / Rsp | – / success, reason |
| 12 | Notification | payload, channel? |
| 15 | ErrorResponse | originalCommand, code, payload |
| 16 | SessionChangeNotification | change, nodesOfInterest? |
| 18 | SessionInitialStateNotification | initialstate |
| 20 / 21 | PingReq / Rsp | times |

Addresses are `macho.MachoAddress` with state `('C', clientID, callID, service)`,
`('B', broadcastID, narrowcast, idtype)`, `('A', service, callID)` or
`('N', nodeID, service, callID)`.

`Response()` swaps source/destination and copies userID and channel. The client
matches responses on `destination.callID`. ErrorResponse codes: 0
UnMachoDestination, 1 UnMachoChannel, 2 wrapped exception (`payload = (exc,)`,
re-raised client-side by ExceptionWrapper.CallDown).

## 5. GPCS payload wrapping

Client stack, wire side first: ExceptionWrapper, ExceptionMapping, ObjectCall,
ServiceCall, BroadcastStuff. The `authentication` channel stops at ObjectCall.

* **Service call**: `CallReq(dest=Any/Node(service=S), payload=(0, marshal((1, method, args, kwargs))))`.
  kwargs always carries `machoVersion`.
* **Bound-object call**: `payload=(1, marshal((objectID, method, args, kwargs)))`.
* **Responses**: `CallRsp.payload` and `AuthenticationRsp.payload` are bare marshal streams.
* **Errors**: ErrorResponse code 2 with `(exception,)`, *not* inner-pickled.
  UserError must use guid `__builtin__.UserError` (`exceptions` is Python's
  builtin module and has no UserError) with state dict `msg`, `dict`, `args`.
* **Object references**: tag 0x19 wrapping `"N=<node>:<oid>"`; the client's
  Load callback (ObjectCall.ParseObjectID) makes a MachoObjectConnection.
* **MachoBindObject**: args `(bindParams, call)`, returns `(boundObject, callResult)`.
* **Events** (server → client Notification): destination `Broadcast(broadcastID=<event>)`,
  payload `(0, (0, marshal((0, (1, args, seq)))))`; seq 0 skips sequencing.

## 6. Login and cached objects

`ConnectToServer`: connect, version check, `AuthenticationReq`, then
`AuthenticationRsp.payload = (initvals, serviceInfo)`. `initvals` maps 23 keys
to `util.CachedObject` stubs with state `(objectID, nodeID, objectVersion[, shared])`.
For each, the client calls `objectCaching.GetCachableObject(shared, objectID,
objectVersion, nodeID)` and gets an `objectCaching.CachedObject` with state
`(version, object, nodeID, shared, pickle, compressed, objectID)`. Versions are
`(blueTime, checksum)`: the client refetches only when the checksum differs and
the time is newer. The tables are described in [static-data.md](static-data.md).

## 7. Session changes

The client's session (script/sys/sessions.py) learns every attribute from the
server: `SessionChangeNotification(change=(0, {attr: (old, new)}))` with oob
`{'channel': 'sessionchange'}`, applied verbatim by ApplyRemoteAttributeChanges
(nothing is derived: send `locationid` with `stationid`). machoNet closes the
transport on a `charid` change whose old value is not None. Changes raised by
a call go out before its response. gameui then switches scenes: `stationid`
set → station; `solarsystemid` + `shipid` → space (michelle adds a ballpark);
`solarsystemid` old → new → the "Jumping" path.

## 8. Bound objects (monikers)

util/moniker.py: `<service>.MachoResolveObject(bindParams, justQuery)` returns
a node ID, then `<service>.MachoBindObject(bindParams, call)` returns
`(objectRef, result of call)`; later calls are object calls on the reference.
Methods that return an object (invbroker.GetInventory) return a new reference.
Bind parameters: invbroker/dogmaIM/ship/skillMgr `(locationID, groupStation |
groupSolarSystem)`, corpStationMgr `stationID`, beyonce `solarsystemID`, chatx
`(channelID, channelType, extra, channelName)`, factory and broker
`stationID`, missionMgr `('courier', stationID)`, market `regionID`.

A reference can also reach a client that did not ask for it: trademgr's
InitiateTrade binds the trade session in the *other* pilot's session (the
`Push::BindEvent` between connections) and sends it as the last argument of
`OnTrade('Initiate', initiatorID, session)`.

Server-raised message keys must exist client-side: either in the shipped
res:/messages.txt or in `config.BulkData.messages`. gen_seed adds every
`user_error("Key", ...)` the server source raises, plus keys the client only
reaches indirectly (facility.py shows whatever key its subclass returns).

## 9. Space (destiny)

Server → client: event `DoDestinyUpdate(state)`, `state` = list of
`(stamp, (funcName, args))`. michelle applies SetState / AddBalls /
RemoveBall(s) itself and everything else as `_parent_<funcName>(*args)` on the
native park. Stamps count `const.simulationTimeStep` ticks; the client's park
adopts SetState's stamp and advances one per tick.

* `SetState(balls, slims, stamp, egoBallID, solItem, damageState)`: balls are
  blue object images (marshal 0x18) of **destiny.ClientBall** v1. The client's
  destiny.dll has a factory for ClientBall (0x288 bytes) but none for Ball. The
  native SetState (FUN_10021200) reads the C++ fields directly. It must arrive
  after the beyonce bind's response, when the client has installed the park.
* Event argument formats (destiny.dll PyArg_ParseTuple): AddBall
  `ifffiiiiiddddddff|i`; GotoDirection, GotoPoint, WarpTo, SetBallPosition,
  SetBallVelocity `iddd|i`; FollowBall and Orbit `ii|di` (send all four: the
  Python mirror BALL_FollowBall/BALL_Orbit requires them); SetSpeedFraction and
  SetMaxSpeed `if|i`; Stop and RemoveBall `i|i`; MissileFollow `iii|i`.
* slims are `foo.SlimItem` (their `__dict__`): itemID, ballID, charID, ownerID,
  groupID, categoryID, typeID, modules, jumps (rows of locationID,
  toCelestialID), securityStatus, corpID.
* The client does not integrate warp: the server streams SetBallPosition each
  tick along `t = 2 ln(peak/3)/3 + max(0, d - 2 peak/3)/peak`,
  `peak = min(1.5 d, warpSpeed AU)`.

Blue object images: tools/blue_image.py is a reader validated on every .blue
file in the archives, and crates/server/src/space/ballimg.rs is the writer.
The reader (blue.dll FUN_1002c6c0) maps image members to the live class by
name, requiring a matching VARTYPE and the member's persist flag (0x10).
Destiny's ball member table is docs/destiny-ball-members.json
(tools/destiny_members.py).
