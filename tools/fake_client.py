"""Scripted stand-in for the build-1000 client's login, for testing the server.

Plays the client's side of machoNet: version check, AuthenticationReq, then
objectCaching.GetCachableObject for every initvals entry (what ConnectToServer
does) and character.GetCharacterInfo (what the character-select screen does).
The marshal codec here is an independent implementation from the blue.dll
notes, so it also cross-checks the Rust one.

usage: py -3 -I tools/fake_client.py [host] [port] [user] [password]
"""
import socket
import struct
import sys
import zlib

# ------------------------------------------------------------------ marshal
NONE, LONG, I32, I16, I8, M1, Z, ONE, F, F0 = 1, 3, 4, 5, 6, 7, 8, 9, 10, 11
SLONG, SEMPTY, SCHAR, SSHORT, STAB, UNI, BUF, TUP, LST, DCT, INST, CB, REF = 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 25, 27


class Inst:
    def __init__(self, guid, state):
        self.guid, self.state = guid, state

    def __repr__(self):
        return f'<{self.guid} {self.state!r}>'


class Ref(str):
    """A tag-0x19 custom value (bound object reference)."""


def enc(v, o):
    if v is None:
        o.append(NONE)
    elif isinstance(v, bool) or isinstance(v, int):
        v = int(v)
        if v in (-1, 0, 1):
            o.append({-1: M1, 0: Z, 1: ONE}[v])
        elif -128 <= v < 128:
            o += bytes([I8]) + struct.pack('<b', v)
        elif -32768 <= v < 32768:
            o += bytes([I16]) + struct.pack('<h', v)
        elif -2**31 <= v < 2**31:
            o += bytes([I32]) + struct.pack('<i', v)
        else:
            o += bytes([LONG]) + struct.pack('<q', v)
    elif isinstance(v, float):
        o += bytes([F0]) if v == 0.0 else bytes([F]) + struct.pack('<d', v)
    elif isinstance(v, str):
        enc(v.encode('latin-1'), o)
    elif isinstance(v, bytes):
        n = len(v)
        if n == 0:
            o.append(SEMPTY)
        elif n == 1:
            o += bytes([SCHAR]) + v
        elif n < 256:
            o += bytes([SSHORT, n]) + v
        else:
            o.append(SLONG); enclen(n, o); o += v
    elif isinstance(v, (tuple, list)):
        o.append(TUP if isinstance(v, tuple) else LST); enclen(len(v), o)
        for x in v:
            enc(x, o)
    elif isinstance(v, dict):
        o.append(DCT); enclen(len(v), o)
        for k, x in v.items():
            enc(x, o); enc(k, o)
    elif isinstance(v, Inst):
        o.append(INST); enc(v.guid, o); enc(v.state, o)
    else:
        raise TypeError(type(v))


def enclen(n, o):
    o += bytes([n]) if n < 255 else b'\xff' + struct.pack('<I', n)


def dumps(v):
    o = bytearray(b'\x7e\0\0\0\0')
    enc(v, o)
    return bytes(o)


def loads(b):
    if b[0] != 0x7e:
        b = zlib.decompress(b)
    (count,) = struct.unpack_from('<i', b, 1)
    end = len(b) - 4 * max(count, 0)
    smap = list(struct.unpack_from(f'<{max(count, 0)}I', b, end))
    shared = [None] * max(count, 0)
    pos = [5]
    nxt = [0]

    def u8():
        pos[0] += 1
        return b[pos[0] - 1]

    def take(n):
        pos[0] += n
        return b[pos[0] - n:pos[0]]

    def ln():
        n = u8()
        return struct.unpack('<I', take(4))[0] if n == 0xFF else n

    def reserve(flag):
        if not flag:
            return None
        s = smap[nxt[0]] - 1
        nxt[0] += 1
        return s

    def obj():
        raw = u8()
        t, flag = raw & 0x3F, raw & 0x40
        if t == NONE: return None
        if t == LONG: return struct.unpack('<q', take(8))[0]
        if t == I32: return struct.unpack('<i', take(4))[0]
        if t == I16: return struct.unpack('<h', take(2))[0]
        if t == I8: return struct.unpack('<b', take(1))[0]
        if t in (M1, Z, ONE): return {M1: -1, Z: 0, ONE: 1}[t]
        if t == F: return struct.unpack('<d', take(8))[0]
        if t == F0: return 0.0
        if t == SLONG: return take(ln())
        if t == SEMPTY: return b''
        if t == SCHAR: return take(1)
        if t == SSHORT: return take(u8())
        if t == STAB: return STRINGS[u8() - 1]
        if t == UNI: return take(ln()).decode('utf-16-le')
        if t in (TUP, LST):
            n = ln(); s = reserve(flag)
            v = [obj() for _ in range(n)]
            v = tuple(v) if t == TUP else v
        elif t == DCT:
            n = ln(); s = reserve(flag); v = {}
            for _ in range(n):
                val = obj(); v[hashable(obj())] = val
        elif t == INST:
            g = obj(); st = obj(); v = Inst(g.decode(), st); s = reserve(flag)
        elif t == BUF:
            v = take(ln()); s = reserve(flag)
        elif t == CB:
            return Ref(obj().decode())
        elif t == 24:
            v = ('blueobject', take(ln())); s = reserve(flag)
        elif t == REF:
            return shared[ln() - 1]
        else:
            raise ValueError(f'tag {t:#x} at {pos[0] - 1}')
        if s is not None:
            shared[s] = v
        return v

    v = obj()
    assert pos[0] == end, (pos[0], end)
    return v


def hashable(k):
    return tuple(k) if isinstance(k, list) else k


STRINGS = open(__import__('os').path.join(__import__('os').path.dirname(__import__('os').path.abspath(__file__)), '..', 'EVE', 'script', 'sys', 'strings.txt'), 'rb').read().split(b'\n')
STRINGS = [s[:-1] for s in STRINGS[:-1]] + [STRINGS[-1]]

# ------------------------------------------------------------------ machoNet
class Wire:
    def __init__(self, host, port):
        self.s = socket.create_connection((host, port), timeout=30)

    def send(self, data):
        self.s.sendall(struct.pack('<I', len(data)) + data)

    def recv(self):
        def exact(n):
            buf = b''
            while len(buf) < n:
                chunk = self.s.recv(n - len(buf))
                if not chunk:
                    raise EOFError
                buf += chunk
            return buf
        (n,) = struct.unpack('<I', exact(4))
        return loads(exact(n))


def addr(kind, *f):
    return Inst('macho.MachoAddress', (kind, *f))


def packet(guid, cmd, src, dst, user, body, oob=None):
    return Inst(guid, (cmd, src, dst, user, body, oob))


def main():
    host, port = (sys.argv[1:2] or ['127.0.0.1'])[0], int((sys.argv[2:3] or ['26000'])[0])
    user, pw = (sys.argv[3:4] or ['tester'])[0], (sys.argv[4:5] or ['secret'])[0]
    w = Wire(host, port)
    challenge = w.recv()
    print('server version:', challenge)
    assert challenge[0] == 170472 and challenge[1] == 40
    w.send(dumps((170472, 40, None, 10.0, 1000, 'Gemini')))

    w.send(dumps(packet('macho.AuthenticationReq', 0, addr('C', 0, 1, None), addr('A', None, None), None,
                        ({'version.number': 10.0, 'version.build': 1000, 'version.project': 'Gemini'}, user, pw),
                        {'channel': 'authentication'})))
    rsp = w.recv()
    assert rsp.guid == 'macho.AuthenticationRsp', rsp
    cmd, src, dst, uid, body, oob = rsp.state
    serverinfo, user_id, role, payload = body[:4]
    initvals, service_info = loads(payload)
    print(f'authenticated: userID={user_id} role={role} oob={oob} initvals={len(initvals)}')

    call_id = [100]

    notifications = NOTES

    def call(service, method, *args, dest_node=None):
        call_id[0] += 1
        inner = dumps((1, method, args, {'machoVersion': 1}))
        dst = addr('N', dest_node, service, None) if dest_node else addr('A', service, None)
        w.send(dumps(packet('macho.CallReq', 6, addr('C', 0, call_id[0], None), dst, user_id, ((0, inner),))))
        while True:
            r = w.recv()
            if r.guid in ('macho.SessionChangeNotification', 'macho.Notification'):
                notifications.append(r)
                continue
            break
        if r.guid == 'macho.ErrorResponse':
            raise RuntimeError(r.state[4])
        assert r.state[2].state[2] == call_id[0], 'response not routed to our callID'
        return loads(r.state[4][0])

    def pump_(seconds):
        w.s.settimeout(seconds)
        try:
            while True:
                NOTES.append(w.recv())
        except (socket.timeout, TimeoutError):
            pass
        finally:
            w.s.settimeout(30)
    PUMP.append(pump_)

    def object_call(ref, method, *args):
        call_id[0] += 1
        inner = dumps((str(ref), method, args, {'machoVersion': 1}))
        w.send(dumps(packet('macho.CallReq', 6, addr('C', 0, call_id[0], None), addr('N', 1, None, None), user_id,
                            ((1, inner),))))
        while True:
            r = w.recv()
            if r.guid in ('macho.SessionChangeNotification', 'macho.Notification'):
                notifications.append(r)
                continue
            break
        if r.guid == 'macho.ErrorResponse':
            raise RuntimeError(r.state[4])
        return loads(r.state[4][0])

    for key, stub in sorted(initvals.items(), key=lambda kv: kv[0]):
        object_id, node_id, version = stub.state[:3]
        co = call('objectCaching', 'GetCachableObject', 1, object_id, version, node_id, dest_node=node_id)
        ver, obj, node, shared, pickle, compressed, oid = co.state
        table = loads(zlib.decompress(pickle) if compressed else pickle)
        if isinstance(table, Inst):
            rows = len(table.state[b'lines'])
            hdr = table.state[b'header']
        else:
            hdr, rows = table[0], len(table[1])
        print(f'  {key.decode():36} {rows:4} rows  header={[h.decode() for h in hdr]}')
    chars = call('character', 'GetCharacterInfo')
    print('characters:', rows_of(chars))
    if '--create' in sys.argv:
        create(call)
        chars = call('character', 'GetCharacterInfo')
        print('characters:', rows_of(chars))
    if '--select' in sys.argv and rows_of(chars):
        char = rows_of(chars)[0]
        ids = [char[b'characterID'], char[b'corporationID']]
        owners = call('config', 'GetMultiOwnersEx', ids)
        print('owners:', owners[1])
        locs = call('config', 'GetMultiLocationsEx', [x for x in char[b'hierarchy'] if x])
        print('locations:', locs[1])
        app = call('character', 'GetCharacterAppearanceList', [char[b'characterID']])
        print('appearance keys:', len(app[0].state[b'header']))
        del notifications[:]
        call('character', 'SelectCharacterID', char[b'characterID'])
        for n in notifications:
            print('notification:', n.guid, n.state[4], n.state[5])
        if '--station' in sys.argv:
            station_entry(call, char, object_call)
    print('LOGIN FLOW OK')


SYSTEM = []


def station_entry(call, char, object_call):
    station = char[b'hierarchy'][0]
    SYSTEM[:] = [char[b'hierarchy'][1]]
    print('item bits:', call('station', 'GetStationItemBits'))
    node = call('invbroker', 'MachoResolveObject', (station, 15), 0)
    broker, _ = call('invbroker', 'MachoBindObject', (station, 15), None)
    print('invbroker bound:', node, broker)
    hangar = object_call(broker, 'GetInventory', 5, None)
    items = object_call(hangar, 'List')
    print('hangar items:', [dict(zip(i.state[b'header'], i.state[b'line'])) for i in items])
    dogma, ship = call('dogmaIM', 'MachoBindObject', (station, 15), ('ShipGetInfo', (), {}))
    for k, rec in ship.items():
        print('ship info', k, 'attrs', len(rec.state[b'attributes']), 'invItem', rec.state[b'invItem'].state[b'line'])
    chars = object_call(dogma, 'CharGetInfo')
    print('char info items:', len(chars))
    types = call('dogmaIM', 'GetAttributeTypes')
    print('attribute types:', len(types.state[b'items']))
    print('channels:', rows_of(call('chatx', 'GetChannels')))
    local, members = call('chatx', 'MachoBindObject', (station, 1, 1, station), ('List', (), {}))
    print('local members:', rows_of(members))
    csm, logon = call('corpStationMgr', 'MachoBindObject', station, ('GetLogonData', (1, 1, 1, 1, 1, 1), {}))
    print('logon keys:', sorted(k.decode() for k in logon.state))
    print('units:', len(call('config', 'GetUnits').state[b'items']))
    if '--services' in sys.argv:
        services_tour(call, object_call, char, station)
    if '--industry' in sys.argv:
        industry_tour(call, object_call, char, station)
    if '--combat' in sys.argv:
        combat_test(call, object_call, station)
        return
    if '--space' in sys.argv:
        space_flight(call, object_call, station)


def services_tour(call, object_call, char, station):
    """The neocom services, each called the way the client's UI calls it."""
    me = char[b'characterID']
    print('wallet:', call('account', 'GetCashBalance', 0), 'keymap:', rows_of(call('account', 'GetKeyMap')))
    call('bookmark', 'BookmarkLocation', station, None, 'Home	my station')
    bms = rows_of(call('bookmark', 'GetBookmarks'))
    print('bookmarks:', bms)
    call('bookmark', 'UpdateBookmark', bms[-1][b'bookmarkID'], None, 'Home2	note')
    voucher = call('bookmark', 'CreateBookmarkVoucher', bms[-1][b'bookmarkID'], 'Home2	note')
    vid = dict(zip(voucher.state[b'header'], voucher.state[b'line']))[b'itemID']
    vobj = call('voucher', 'GetObject', vid)
    print('voucher:', object_call(vobj, 'GetDescription'), object_call(vobj, 'GetTypeInfo'))
    print('bookmark from voucher:', call('bookmark', 'AddBookmarkFromVoucher', vid))
    call('bookmark', 'DeleteBookmark', bms[-1][b'bookmarkID'])
    call('pager', 'Page', [me], 'Hello:::', 'A message to myself', [])
    pages = rows_of(call('pager', 'GetPages'))
    print('pages:', pages[-1:])
    print('page:', call('pager', 'GetPageMessage', pages[-1][b'pageID']))
    print('find owner:', rows_of(call('pager', 'FindOwner', char[b'characterName'][:3].decode(), 0, 1)))
    call('pager', 'DeletePages', [pages[-1][b'pageID']])
    print('petition categories:', rows_of(call('petitioner', 'GetCategories')))
    pid = call('petitioner', 'CreatePetition', 'Stuck', 'I am stuck', 2, {'Solar System': 'Jita'})
    print('my petitions:', [ (p.state[b'line'][0], p.state[b'line'][3]) for p in call('petitioner', 'GetMyPetitions')])
    call('petitioner', 'PetitionerChat', pid, 'hello?')
    print('petition messages:', rows_of(call('petitioner', 'GetPetitionMessages', pid)))
    call('petitioner', 'CancelPetition', pid)
    chans = rows_of(call('newsmgr', 'GetChannels'))
    call('newsmgr', 'NewThread', chans[0][b'channelID'], 'First', 'The first thread')
    threads = rows_of(call('newsmgr', 'GetThreads', chans[0][b'channelID'], 1, 10))
    call('newsmgr', 'NewResponse', threads[0][b'threadID'], 'A reply')
    print('news responses:', rows_of(call('newsmgr', 'GetResponses', threads[0][b'threadID'], 1, 10)))
    call('standingmgr', 'SetPrivateStanding', me, 5, 'myself')
    print('standing:', call('standingmgr', 'GetPrivateStanding', me).state)
    print('towards me:', rows_of(call('standing', 'GetStandingsTowardsOwner')))
    corp = call('corpmgr', 'GetPublicInfo', char[b'corporationID'])
    print('corp:', dict(zip(corp.state[b'header'], corp.state[b'line']))[b'corporationName'])
    print('roles:', [r[b'roleName'] for r in rows_of(call('corpmgr', 'GetRoles'))])
    print('all corps:', len(rows_of(call('corpmgr', 'GetCorporations'))))
    print('can create:', call('corpmgr', 'UserCanCreateCorporation'), 'cost:', call('corpmgr', 'GetCostForCreatingACorporation'))
    print('tickers:', rows_of(call('corpmgr', 'GetSuggestedTickerNames', 'Genesis Test Corp')))
    print('gang:', call('gangsta', 'GetGangInfoForChar'))
    print('home:', call('character', 'GetHomeStation'), 'visits:', rows_of(call('character', 'GetSolarSystemVisits')))
    print('station count systems:', len(call('character', 'GetStationCount')))
    print('top bounties:', rows_of(call('charmgr', 'GetTopBounties')))
    print('find station:', rows_of(call('station', 'FindLocation', 'a', 0, 15))[:2])
    print('paint shapes:', len(call('station', 'Paintshop_GetLayerShapes')), 'colors:', len(call('station', 'Paintshop_GetLayerColors')))
    skill_mgr, skills = call('skillMgr', 'MachoBindObject', (station, 15), ('CharGetSkills', (), {}))
    print('skills:', skills)
    print('required skills of skill:', call('dogmaIM', 'GetRequiredSkillLevels', list(skills)[0] if skills else 0))
    print('cluster stats:', call('machoNet', 'GetClusterSessionStatistics'))
    print('SERVICES OK')


def industry_tour(call, object_call, char, station):
    """Agents, the star map, reprocessing, repair, factory, brokers, couriers.
    Needs a GM account (slash /create and /giveisk)."""
    me = char[b'characterID']
    slash = lambda line: print(' ', line, '->', call('slash', 'SlashCmd', line))
    broker, _ = call('invbroker', 'MachoBindObject', (station, 15), None)
    hangar = object_call(broker, 'GetInventory', 5, None)
    def hangar_items():
        return [dict(zip(i.state[b'header'], i.state[b'line'])) for i in object_call(hangar, 'List')]
    types = {}
    co = call('config', 'GetMultiInvTypesEx', list(range(1, 2000)))
    for line in co[1]:
        row = dict(zip(co[0], line))
        types[row[b'typeName'].decode()] = row[b'typeID']
    slash('/giveisk 2000000')

    # Agents
    agents = rows_of(call('newBreedAgents', 'GetMyAgents'))
    print('agents here:', [(a[b'agentName'], a[b'level']) for a in agents])
    agent = agents[0][b'charID']
    print('pubinfo of agent:', call('charmgr', 'GetPublicInfo', agent).state[b'line'][1])
    for cmd in ('root', 'mission::ask'):
        dialogue, public, cookie = call('newBreedAgents', 'Interact', agent, cmd, None, None)
        print(' agent', cmd, '->', dialogue, sorted(public))
    dialogue, public, _ = call('newBreedAgents', 'Interact', agent, 'mission::accept', None, None)
    status = call('newBreedAgents', 'GetMyMissionStatus')
    for k, v in status.items():
        print('journal:', k, v[2][b'name'], v[2][b'items.fetch.1.typeandquantity'])
        type_id, qty = v[2][b'items.fetch.1.typeandquantity'][1:]
    is_mining = type_id != types.get('Tourists')
    if is_mining:
        slash(f'/create {type_id} {qty}')
        print(' complete ->', call('newBreedAgents', 'Interact', agent, 'mission::complete', None, None)[0])
    else:
        print(' quit ->', call('newBreedAgents', 'Interact', agent, 'mission::quit', None, None)[0])
    print('wallet after agent:', call('account', 'GetCashBalance', 0))

    # Star map
    regions = rows_of(call('config', 'GetMapObjects', 8, 1))
    system = char[b'hierarchy'][1]
    print('map regions:', len(regions), 'connections from here:', len(call('config', 'GetMapConnections', system, 1, 1, 1, 0, 1)),
          'landmarks:', len(rows_of(call('config', 'GetMapLandmarks'))))
    items = rows_of(call('config', 'GetMap', system))
    sun = [i for i in items if i[b'itemName'].endswith(b'Sun') or i[b'orbitID'] is None]
    print('system items:', len(items), 'sun stats:', rows_of(call('config', 'GetCelestialStatistic', sun[0][b'itemID'])) if sun else None)

    # Reprocessing
    ore = types['Asteroid 1']
    slash(f'/create {ore} 450')
    rock = max((i for i in hangar_items() if i[b'typeID'] == ore), key=lambda i: i[b'quantity'])
    print('reprocess options:', call('reprocessing', 'GetOptionsForItemTypes', {ore: 0}))
    q = call('reprocessing', 'GetQuote', rock[b'itemID'])
    print('quote:', q.state[b'quantityToProcess'], [(r.state[b'typeID'], r.state[b'client'], r.state[b'station']) for r in q.state[b'recoverables']])
    call('reprocessing', 'Reprocess', rock[b'itemID'])
    print('after refining:', [(i[b'typeID'], i[b'quantity']) for i in hangar_items() if i[b'typeID'] in types.values() and i[b'quantity'] > 1])

    # Repair
    ship = next(i for i in hangar_items() if i[b'categoryID'] == 10 or i[b'singleton'] == 1 and i[b'quantity'] == 1 and i[b'flag'] == 4)
    call('repair', 'DamageModule', ship[b'itemID'], 40)
    rep = call('repair', 'GetDamageReport', ship[b'itemID'])
    print('damage report:', [(r.state[b'typeID'], r.state[b'damage'], r.state[b'maxHealth']) for r in rep.state[b'quote']])
    call('repair', 'RepairShip', ship[b'itemID'], 1000)
    print('after repair:', [r.state[b'damage'] for r in call('repair', 'GetDamageReport', ship[b'itemID']).state[b'quote']])

    # Factory: rent a slot, install a blueprint, look at the job, eject it.
    bp_type = types['Miner I Blueprint']
    slash(f'/create {bp_type} 1')
    slash('/create Industry 1')
    skill = next(i for i in hangar_items() if i[b'typeID'] == types['Industry'])
    me_inv = object_call(broker, 'GetInventoryFromId', me)
    try:
        object_call(me_inv, 'Add', skill[b'itemID'])
    except RuntimeError as e:
        print('inject failed:', e)
    bp = next(i for i in hangar_items() if i[b'typeID'] == bp_type)
    print('bp attributes:', call('factory', 'GetBlueprintAttributes', bp[b'itemID']))
    print('bp info:', call('factory', 'GetBlueprintInformation', bp[b'itemID']).state)
    product = call('factory', 'GetBlueprintAttributes', bp[b'itemID'])[b'productTypeID']
    mats = rows_of(call('factory', 'GetMineralCompositionOfItemType', product))
    print('materials:', mats)
    for m in mats:
        slash(f'/create {m[b"typeID"]} {m[b"quantity"] * 3}')
    csm, _ = call('corpStationMgr', 'MachoBindObject', station, None)
    print('slots for rent:', object_call(csm, 'AreAnyFactorySlotsAvailableForRenting', 0), 'quote:', object_call(csm, 'GetQuoteForRentingAFactorySlot', 0))
    slot = object_call(csm, 'RentFactorySlot', 10000.0, 0)
    fac, slots = call('factory', 'MachoBindObject', station, ('GetFactorySlots', (), {}))
    print('rented slot', slot, 'slots:', [(s.state[b'slotID'], s.state[b'canUse']) for s in slots][:3])
    factory_inv = object_call(broker, 'GetInventory', 3, None)
    try:
        object_call(factory_inv, 'Add', bp[b'itemID'], 1, slot, 2, 3)
        info = object_call(fac, 'GetFactorySlot', slot).state
        status = object_call(fac, 'GetFactoryStatus', info[b'installedItem'].state[b'line'][0])
        print('job:', {k.decode(): v for k, v in status.items() if k in (b'manufacturing', b'secondsTotal', b'iterationsRequested')})
        object_call(fac, 'EjectItemInSlot', slot, 'test', 1)
        print('ejected; blueprint back:', any(i[b'itemID'] == bp[b'itemID'] for i in hangar_items()))
    except RuntimeError as e:
        print('install failed:', e)
    object_call(csm, 'CancelRentOfFactorySlot', slot)

    # Couriers: pack a mineral stack for another station, list, cancel.
    mineral = next(i for i in hangar_items() if i[b'typeID'] == mats[0][b'typeID'])
    mis, opts = call('missionMgr', 'MachoBindObject', ('courier', station), ('GetCourierOptions', (), {}))
    print('courier options: items', len(opts.state[b'itemList'].state[b'items']), 'fee', opts.state[b'basicFee'])
    dest = next(s for s in rows_of(call('station', 'FindLocation', 'a', 0, 15)) if s[b'locationID'] != station)[b'locationID']
    print('created courier:', object_call(mis, 'CreateCourierMission', [(mineral[b'itemID'], None)], dest, None, 0.0, 0, 5000.0, 100.0))
    mine = rows_of(call('missionMgr', 'GetMyCourierMissions'))
    print('my couriers:', [(m[b'missionID'], m[b'state'], m[b'dropStationID']) for m in mine])
    call('missionMgr', 'CancelMission', mine[-1][b'missionID'])
    print('after cancel:', len(rows_of(call('missionMgr', 'GetMyCourierMissions'))), 'mineral back:', any(i[b'itemID'] == mineral[b'itemID'] for i in hangar_items()))

    # Brokers: sign up, allow minerals, let the ticker list them.
    brk, brokers = call('broker', 'MachoBindObject', station, ('GetBrokersAtStation', (), {}))
    b = rows_of(brokers)[0][b'brokerid']
    object_call(brk, 'Signup', b)
    mineral_group = call('config', 'GetMultiInvTypesEx', [mats[0][b'typeID']])
    mgroup = dict(zip(mineral_group[0], mineral_group[1][0]))[b'groupID']
    object_call(brk, 'AddRuleForClient', b, 0, mgroup, 0, 1)
    print('broker rules:', rows_of(object_call(brk, 'GetRulesForClient', b)), 'with broker:', object_call(brk, 'IsCharWithBroker', b))
    pump(7.0)
    mkt, _ = call('market', 'MachoBindObject', char[b'hierarchy'][3] if len(char[b'hierarchy']) > 3 else None, None)
    orders = object_call(mkt, 'CharGetOrders')
    print('broker orders:', orders)
    object_call(brk, 'Resign', b)
    print('INDUSTRY OK')


def wait_events(n_timeout=25):
    import time
    end = time.time() + n_timeout


def space_flight(call, object_call, station):
    import importlib.util, os, time
    spec = importlib.util.spec_from_file_location('bi', os.path.join(os.path.dirname(os.path.abspath(__file__)), 'blue_image.py'))
    bi = importlib.util.module_from_spec(spec); spec.loader.exec_module(bi)
    ship, _ = call('ship', 'MachoBindObject', (station, 15), None)
    del NOTES[:]
    object_call(ship, 'Undock')
    change = [n for n in NOTES if n.guid == 'macho.SessionChangeNotification']
    sysid = change[-1].state[4][0][1][b'solarsystemid'][1]
    print('undocked into', sysid)
    del NOTES[:]
    park, _ = call('beyonce', 'MachoBindObject', sysid, None)
    pump(1.0)
    state = destiny_batches(NOTES)[0][0]
    stamp, (fn, args) = state
    balls, slims = args[0], args[1]
    decoded = [bi.parse(b[1])[2] for b in balls]
    print(fn.decode(), 'stamp', stamp, 'balls', len(decoded), 'ego', args[3])
    gates = [sl.state for sl in slims if sl.state[b'jumps']]
    gate = gates[0]
    print('first gate', gate[b'itemID'], gate[b'name'], [dict(zip(j.state[b'header'], j.state[b'line'])) for j in gate[b'jumps']])
    del NOTES[:]
    object_call(park, 'WarpToItem', gate[b'itemID'])
    t0 = time.time()
    while time.time() - t0 < 120:
        pump(1.5)
        evs = [e for b in destiny_batches(NOTES) for e in b]
        if any(e[1][0] == b'Stop' for e in evs):
            break
    evs = [e for b in destiny_batches(NOTES) for e in b]
    print('warp events', len(evs), 'last', evs[-1][1][0] if evs else None, 'after %.0fs' % (time.time() - t0))
    del NOTES[:]
    dest = gate[b'jumps'][0].state[b'line']
    object_call(park, 'StargateJump', gate[b'itemID'], dest[1])
    change = [n for n in NOTES if n.guid == 'macho.SessionChangeNotification']
    print('jump session changes:', [list(c.state[4][0][1].keys()) for c in change])
    newsys = change[-1].state[4][0][1][b'solarsystemid'][1]
    del NOTES[:]
    ref, _ = call('beyonce', 'MachoBindObject', newsys, None)
    LAST_BIND[:] = [ref]
    pump(1.0)
    st = destiny_batches(NOTES)[0][0]
    print('arrived in', newsys, 'SetState balls', len(st[1][1][0]))
    slims2 = st[1][1][1]
    stations = [x.state for x in slims2 if x.state[b'groupID'] == gate_group_station(slims2)]
    park2 = Ref('N=1:%d' % 0)
    if stations:
        target = stations[0][b'itemID']
        print('warping to station', stations[0][b'name'])
        park2 = LAST_BIND[0]
        del NOTES[:]
        object_call(park2, 'WarpToItem', target)
        t0 = time.time()
        while time.time() - t0 < 180:
            pump(1.5)
            if any(e[1][0] == b'Stop' for b in destiny_batches(NOTES) for e in b):
                break
        del NOTES[:]
        object_call(park2, 'Dock', target)
        change = [n for n in NOTES if n.guid == 'macho.SessionChangeNotification']
        print('docked:', change[-1].state[4][0][1] if change else 'no session change')


def combat_test(call, object_call, station):
    """Undock, warp to a belt asteroid, lock it and mine one cycle."""
    import time
    if station is not None:
        ship, _ = call('ship', 'MachoBindObject', (station, 15), None)
        del NOTES[:]
        object_call(ship, 'Undock')
        change = [n for n in NOTES if n.guid == 'macho.SessionChangeNotification']
        sysid = change[-1].state[4][0][1][b'solarsystemid'][1]
    else:
        sysid = SYSTEM[0]
    del NOTES[:]
    park, _ = call('beyonce', 'MachoBindObject', sysid, None)
    pump(1.0)
    st = destiny_batches(NOTES)[0][0]
    slims = st[1][1][1]
    shipid = st[1][1][3]
    rocks = [x.state for x in slims if 1900000000 <= x.state[b'itemID'] < 2000000000]
    pirates = [x.state for x in slims if x.state[b'itemID'] >= 2000000000]
    print('asteroids', len(rocks), 'pirates', len(pirates), 'ship modules', [x.state[b'modules'] for x in slims if x.state[b'itemID'] == shipid])
    if not rocks:
        return
    rock = rocks[0]
    print('warping to', rock[b'name'], rock[b'itemID'])
    del NOTES[:]
    try:
        object_call(park, 'WarpToItem', rock[b'itemID'])
    except RuntimeError as e:
        print('no warp needed:', e)
    t0 = time.time()
    while time.time() - t0 < 150:
        pump(1.5)
        if any(e[1][0] == b'Stop' for b in destiny_batches(NOTES) for e in b):
            break
    dogma, _ = call('dogmaIM', 'MachoBindObject', (sysid, 5), None)
    del NOTES[:]
    object_call(dogma, 'AddTarget', rock[b'itemID'])
    pump(3.0)
    events = [n.state[1].state[1] for n in NOTES if n.guid == 'macho.Notification']
    print('target events:', events)
    broker, _ = call('invbroker', 'MachoBindObject', (sysid, 5), None)
    inv = object_call(broker, 'GetInventoryFromId', shipid)
    items = [dict(zip(i.state[b'header'], i.state[b'line'])) for i in object_call(inv, 'List')]
    miner = [i for i in items if i[b'flag'] == 28]
    print('ship items:', [(i[b'typeID'], i[b'flag'], i[b'quantity']) for i in items])
    if miner:
        del NOTES[:]
        object_call(dogma, 'Activate', miner[0][b'itemID'], 'mining', rock[b'itemID'], 1)
        pump(3.0)
        names = [n.state[1].state[1] for n in NOTES if n.guid == 'macho.Notification']
        print('after mining:', names)
        items = [dict(zip(i.state[b'header'], i.state[b'line'])) for i in object_call(inv, 'List')]
        print('cargo now:', [(i[b'typeID'], i[b'quantity']) for i in items if i[b'flag'] == 5])
    if pirates:
        # Fly to a pirate: it locks and shoots back.
        del NOTES[:]
        object_call(park, 'FollowBall', pirates[0][b'itemID'], 2000.0)
        pump(40.0)
        ds = [e for b in destiny_batches(NOTES) for e in b if e[1][0] in (b'OnDamageStateChange', b'OnSpecialFX', b'Orbit')]
        names = sorted({n.state[1].state[1] for n in NOTES if n.guid == 'macho.Notification'}, key=str)
        print('pirate activity:', [(e[1][0], e[1][1][:2]) for e in ds][:8], 'notifications:', names)
        gun = [i for i in items if i[b'flag'] == 27]
        p = pirates[0][b'itemID']
        del NOTES[:]
        try:
            object_call(dogma, 'AddTarget', p)
            pump(3.0)
            object_call(dogma, 'Activate', gun[0][b'itemID'], 'targetAttack', p, 0)
            pump(8.0)
            ds = [e[1][0] for b in destiny_batches(NOTES) for e in b]
            print('shooting:', sorted(set(ds), key=str))
        except RuntimeError as e:
            print('shooting failed:', e)
    print('COMBAT OK')


def gate_group_station(slims):
    for x in slims:
        if b'Station' in (x.state.get(b'name') or b''):
            return x.state[b'groupID']
    return -1


def destiny_batches(notes):
    out = []
    for n in notes:
        if n.guid != 'macho.Notification':
            continue
        if n.state[2].state[1] != b'DoDestinyUpdate':
            continue
        ew = n.state[4][0]
        sc = loads(ew[1][1])
        out.append(sc[1][1][0])
    return out


NOTES = []
PUMP = []
LAST_BIND = []


def pump(seconds):
    PUMP[0](seconds)


def rows_of(rowset):
    st = rowset.state
    return [dict(zip(st[b'header'], line)) for line in st[b'lines']]


def create(call):
    info = call('character', 'GetCharCreationInfo')
    app = call('character', 'GetAppearanceInfo')
    print('creation keys:', sorted(k.decode() for k in info))
    print('appearance families:', {k.decode(): len(v.state[b'lines']) for k, v in app.items()})
    t = {k.decode(): rows_of(v) for k, v in info.items()}
    bl = t['bloodlines'][0]
    anc = next(a for a in t['ancestries'] if a[b'ancestryID'] and a[b'bloodlineID'] == bl[b'bloodlineID'])
    school = next(s for s in t['schools'] if s[b'raceID'] == bl[b'raceID'])
    dept = next(d for d in t['departments'] if d[b'schoolID'] == school[b'schoolID'])
    field = next(f for f in t['fields'] if f[b'departmentID'] == dept[b'departmentID'])
    spec = next(s for s in t['specialities'] if s[b'fieldID'] == field[b'fieldID'])
    gender = 1
    appearance = {}
    for fam, stem in [('skins', 'skin'), ('hairs', 'hair'), ('eyes', 'eyes'), ('costumes', 'costume')]:
        opts = [o for o in t[fam] if o[b'bloodlineID'] == bl[b'bloodlineID'] and o[b'gender'] == gender]
        if opts:
            appearance[stem + 'ID'] = opts[0][(stem + 'ID').encode()]
    appearance.update({'morph1e': 0.1, 'camPos1': 0.0, 'headRotation1': 0.2})
    name = 'Tester %d' % (__import__('random').randrange(1000, 9999))
    char_id = call('character', 'CreateCharacter', name, bl[b'bloodlineID'], gender, anc[b'ancestryID'],
                   school[b'schoolID'], dept[b'departmentID'], field[b'fieldID'], spec[b'specialityID'],
                   1, 1, 1, 1, 1, appearance)
    print('created', name, char_id)


if __name__ == '__main__':
    main()
