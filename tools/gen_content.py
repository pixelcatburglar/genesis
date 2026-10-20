"""Generate db/static/04_content.sql: the world and character-creation content.

REAL (shipped by the client):
  * the universe map - every region, constellation and solar system with its
    name, position, bounding box, security, luminosity and faction, the
    stargate graph and the map landmarks - decoded from
    res:/UI/Shared/Maps/cachefile.blue, a triui.UIWindow whose `data` member is
    a marshal stream (ui/shared/maps/map.py CreateCacheFile wrote it from the
    original server's config.GetMapObjects / GetMapConnections / GetMapLandmarks).
  * appearance options: res/ui/Login/facetree.txt cross-checked against the
    face models in resFace.stuff.
  * every graphic below points at a model or icon that exists in the archives.

TEMPLATE DEFAULT (authored here, replaceable):
  * faction/corporation/school/career names and descriptions, base attributes,
    skills, ship types; celestials inside each system (sun, planets, moons,
    stargates, stations) are generated deterministically from the system ID.

usage: py -3 -I tools/gen_content.py > db/static/04_content.sql
"""
import hashlib
import importlib.util
import json
import math
import os
import random
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(ROOT, 'tools', name + '.py'))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


seed = load('gen_seed')      # client-contract IDs
fc = load('fake_client')     # independent marshal reader
words, q = seed.words, seed.q
ASSET_NAMES = [json.loads(l)['path'] for l in open(os.path.join(ROOT, 'client_res', 'index.jsonl'))]
ASSETS = {p.lower() for p in ASSET_NAMES}

out = []


def emit(table, cols, rows, chunk=500):
    rows = list(rows)
    for i in range(0, len(rows), chunk):
        out.append(f'INSERT INTO static.{table} ({", ".join(chr(34) + c + chr(34) for c in cols)}) VALUES')
        out.append(',\n'.join('    (' + ', '.join(q(v) for v in r) + ')' for r in rows[i:i + chunk]) + ';\n')


def asset(path):
    assert path.lower().replace('res:/', 'res/') in ASSETS, f'missing asset {path}'
    return path


# ===================================================================== IDs
# Content groups/types live above the client-contract ranges.
next_group = [100]
next_type = [1000]
next_graphic = [1]
groups, types, graphics = [], [], []
group_ids = dict(seed.group_id)
type_ids = dict(seed.type_id)
cat = seed.cat_id


# graphicIDs 416..512 are corporation logo shapes: CreateCorp and uix.GetCorpLogo
# pick random.randint(416, 512) (editcorpdetails.py, uix.py). They and the
# paintshop colours are laid out below; content graphics skip that block.
LOGO_SHAPE_FIRST = 416
RESERVED = set(range(LOGO_SHAPE_FIRST, 600)) | set(range(391, 396))  # + engine flares (gear section)


def graphic(url3D=None, icon=None):
    while next_graphic[0] in RESERVED:
        next_graphic[0] += 1
    gid = next_graphic[0]
    next_graphic[0] += 1
    graphics.append((gid, url3D and asset(url3D), icon and asset(icon), None))
    return gid


def icon(sheet, n):
    return graphic(icon=f'res:/UI/Icon/Mapped/icon{sheet:02d}_{n:02d}.blue')


def corp_logo(sheet, n):
    """A graphic whose icon is 'c_<sheet>_<quadrant>': uix.GetIcon hands it to
    MapCorpLogo, which maps quadrant n (1..4, row-major) of corpsNN.dds."""
    asset(f'res:/UI/Texture/Corps/corps{sheet:02d}.dds')
    while next_graphic[0] in RESERVED:
        next_graphic[0] += 1
    gid = next_graphic[0]
    next_graphic[0] += 1
    graphics.append((gid, None, f'c_{sheet}_{n}', None))
    return gid


# The paintshop/corporation logo library (station.Paintshop_GetLayerShapes /
# GetLayerColors): res:/UI/Station/Paintshop/Lib/LayerShape/<n>.blue are the
# shapes (graphicID 416 + n), LayerColor/<name>.blue the colours after them.
LAYER_SHAPES = sorted(int(p.rsplit('/', 1)[1][:-5]) for p in ASSETS
                      if p.startswith('res/ui/station/paintshop/lib/layershape/'))
LAYER_COLORS = sorted(p.rsplit('/', 1)[1][:-5] for p in ASSETS
                      if p.startswith('res/ui/station/paintshop/lib/layercolor/'))
logo_shapes, logo_colors = [], []
for n in LAYER_SHAPES:
    path = asset(f'res:/UI/Station/Paintshop/Lib/LayerShape/{n}.blue')
    graphics.append((LOGO_SHAPE_FIRST + n, path, None, None))
    logo_shapes.append((LOGO_SHAPE_FIRST + n, path))
for i, name in enumerate(LAYER_COLORS):
    original = next(p for p in ASSET_NAMES if p.lower() == f'res/ui/station/paintshop/lib/layercolor/{name}.blue')
    path = 'res:/' + original[len('res/'):]
    gid = LOGO_SHAPE_FIRST + len(LAYER_SHAPES) + i
    graphics.append((gid, path, None, None))
    logo_colors.append((gid, path, original.rsplit('/', 1)[1][:-5]))
assert LOGO_SHAPE_FIRST + len(LAYER_SHAPES) + len(LAYER_COLORS) <= 600


def group(name, category):
    if name in group_ids:
        return group_ids[name]
    gid = next_group[0]
    next_group[0] += 1
    groups.append((gid, cat[category], words(name)))
    group_ids[name] = gid
    return gid


def typ(name, group_name, **kw):
    tid = next_type[0]
    next_type[0] += 1
    types.append(dict(typeID=tid, groupID=group_ids[group_name], typeName=name, **kw))
    type_ids[name] = tid
    return tid


# ============================================================ the universe
blob = open(os.path.join(ROOT, 'client_res', 'res', 'ui', 'Shared', 'Maps', 'cachefile.blue'), 'rb').read()
# UIWindow image: the `data` member is a marshal stream at 0x70 whose length is
# the dword at 0x6A (the 847-entry share map ends exactly there).
universe = fc.loads(blob[0x70:0x70 + struct.unpack_from('<I', blob, 0x6A)[0]])
items = universe[b'items']
MAP_COLS = [h.decode() for h in next(iter(items.values()))[0].state[b'header']]


def row(entry):
    return dict(zip(MAP_COLS, entry[0].state[b'line']))


def text(v):
    return v.decode('latin-1') if isinstance(v, bytes) else v


objects = {k: row(e) for k, e in items.items()}
for o in objects.values():
    o['itemName'] = text(o['itemName'])
regions = {k: o for k, o in objects.items() if o['groupID'] == 3}
constellations = {k: o for k, o in objects.items() if o['groupID'] == 4}
systems = {k: o for k, o in objects.items() if o['groupID'] == 5}
assert (len(regions), len(constellations), len(systems)) == (66, 780, 5334)

jumps = set()
for sid, e in items.items():
    if len(e) > 2:
        for lst in e[2]:
            for to in lst:
                jumps.add((sid, to))
                jumps.add((to, sid))
neighbours = {}
for a, b in jumps:
    neighbours.setdefault(a, []).append(b)

constellation_of = {sid: o['locationID'] for sid, o in systems.items()}
region_of = {cid: o['locationID'] for cid, o in constellations.items()}

emit('map_objects', MAP_COLS, [[o[c] for c in MAP_COLS] for _, o in sorted(objects.items())])
emit('map_jumps', ['fromSolarSystemID', 'toSolarSystemID'], sorted(jumps))
emit('map_landmarks', ['landmarkID', 'landmarkName', 'description', 'radius', 'x', 'y', 'z', 'importance'],
     [(k, text(v[0]), text(v[1]), v[2], v[3], v[4], v[5], v[6]) for k, v in sorted(universe[b'landmarks'].items())])

# ================================================================ factions
# TEMPLATE DEFAULT names; the IDs and the regions each holds are real.
# Region ownership confirms the mapping (e.g. The Forge -> 500001,
# Heimatar -> 500002, Domain -> 500003, Sinq Laison -> 500004).
RACE = {'Caldari': 1, 'Minmatar': 2, 'Amarr': 4, 'Gallente': 8, 'Jove': 16}
FACTIONS = {
    500001: ('Caldari State', 'Caldari'), 500002: ('Minmatar Republic', 'Minmatar'),
    500003: ('Amarr Empire', 'Amarr'), 500004: ('Gallente Federation', 'Gallente'),
    500005: ('Jove Empire', 'Jove'), 500007: ('Ammatar Mandate', 'Amarr'),
    500008: ('Khanid Kingdom', 'Amarr'), 500009: ('The Syndicate', 'Gallente'),
    500010: ('Guristas Pirates', 'Caldari'), 500011: ('Angel Cartel', 'Minmatar'),
    500014: ('ORE', 'Gallente'), 500015: ('Thukker Tribe', 'Minmatar'),
    500019: ("Sansha's Nation", 'Amarr'),
}
assert {o['factionID'] for o in systems.values()} - {None} == set(FACTIONS)
emit('factions', ['factionID', 'factionName', 'raceIDs'],
     [(f, n, RACE[r]) for f, (n, r) in sorted(FACTIONS.items())])

# ============================================================== graphics/types
group('Sun', 'Celestial')
# A sun type's graphic is its LENS FLARE: sun.py LoadModel copies it and adds
# occluders, and takes the sun model (Model/WorldObject/Sun/<name>) and the
# light (Model/Sunlight/<name>) from the same file name.
SUNS = ['yellow', 'orange', 'Red', 'white', 'blue', 'smallYellow', 'orangesun', 'radiatingorange']
for s in SUNS:
    for d in ('Model/WorldObject/Sun', 'Model/Sunlight'):
        assert any(p.lower() == f'res/{d}/{s}.blue'.lower() for p in ASSET_NAMES), (d, s)
lensflare = {p.lower(): 'res:/' + p[4:] for p in ASSET_NAMES if p.lower().startswith('res/model/lensflare/')}
sun_types = [typ(f'Sun {words(s[0].upper() + s[1:])}', 'Sun',
                 graphicID=graphic(lensflare[f'res/model/lensflare/{s.lower()}.blue'])) for s in SUNS]
planet_types = [type_ids['PlanetGas'], type_ids['PlanetIce'], type_ids['PlanetSolid']]
type_ids['Asteroid Belt'] = typ('Asteroid Belt', 'AsteroidBelt',
                                graphicID=graphic('res:/Model/WorldObject/AsteroidField/asteroidCircle.blue'))
GATE_MODEL = {'Amarr': 'res:/Model/Jumpgate/amarr/aj1.blue', 'Caldari': 'res:/Model/Jumpgate/Caldari/cj1/cj1.blue',
              'Gallente': 'res:/Model/Jumpgate/Gallente/gj1/gj1.blue', 'Minmatar': 'res:/Model/Jumpgate/Minmatar/mj3/mj3.blue'}
gate_types = {r: typ(f'{r} Stargate', 'Stargate', graphicID=graphic(m)) for r, m in GATE_MODEL.items()}
# Ball radii measured from the models' meshes (.tri bounding sphere at 0x1e /
# bbox at 0x2e): the client draws stations and gates at the artist's scale
# but computes dock/jump range (surfaceDist) from the radius we send.
GATE_RADIUS = {'Amarr': 4500.0, 'Caldari': 4400.0, 'Gallente': 4200.0, 'Minmatar': 3200.0}

# Station types: hull model + the race's hangar scene (ui/station/base.py Setup).
group('Station', 'Station')
STATION_MODEL = {'Amarr': 'res:/Model/Station/Amarr/as_standard.blue',
                 'Caldari': 'res:/Model/Station/Caldari/cs1/cs1.blue',
                 'Gallente': 'res:/Model/Station/Gallente/gs1/gs1.blue',
                 'Minmatar': 'res:/Model/Station/Minmatar/MS_standard.blue',
                 'Jove': 'res:/Model/Station/Jove/js2/js2.blue'}
HANGAR = {'Amarr': 'Amarr', 'Caldari': 'Caldari', 'Gallente': 'GallenteBlue', 'Minmatar': 'Minmatar', 'Jove': 'Concord'}
# TEMPLATE DEFAULT for the modular Amarr/Minmatar/Jove stations, whose modules
# are placed by scene-graph transforms the mesh headers do not include.
STATION_RADIUS = {'Amarr': 15000.0, 'Caldari': 12400.0, 'Gallente': 7500.0, 'Minmatar': 15000.0, 'Jove': 18200.0}
station_types, station_type_rows = {}, []
for race, model in STATION_MODEL.items():
    tid = typ(f'{race} Station', 'Station', graphicID=graphic(model))
    station_types[race] = tid
    station_type_rows.append((tid, graphic(f'res:/Scene/Hangar/{HANGAR[race]}.blue'), RACE[race]))
emit('station_types', ['stationTypeID', 'hangarGraphicID', 'raceID'], station_type_rows)

# Ships: one rookie hull per race, plus the capsule.
type_ids['Capsule'] = typ('Capsule', 'Capsule', graphicID=graphic('res:/Model/WorldObject/Capsule/capsule.blue'),
                          volume=500.0, capacity=0.0)
# TEMPLATE DEFAULT: what jettisoned cargo floats in (ship.Jettison) and what a
# destroyed ship leaves behind.
type_ids['Cargo Container'] = typ('Cargo Container', 'CargoContainer',
                                  graphicID=graphic('res:/Model/WorldObject/Cargo/cargoContainer.blue'),
                                  volume=1000.0, capacity=27500.0)
ROOKIE = {'Amarr': ('Impairor', 'res:/Model/Ship/Amarr/af1/af1.blue'),
          'Caldari': ('Ibis', 'res:/Model/Ship/Caldari/cf1/cf1.blue'),
          'Gallente': ('Velator', 'res:/Model/Ship/Gallente/gf1/gf1.blue'),
          'Minmatar': ('Reaper', 'res:/Model/Ship/Minmatar/mf1/mf1.blue')}
rookie_ship = {r: typ(n, 'Rookieship', graphicID=graphic(m), volume=27000.0, capacity=135.0)
               for r, (n, m) in ROOKIE.items()}

# Skills (category Skill). The client requires types Industry, Science,
# Corporation Management, Duplicating and Reverse Engineering by name.
SKILL_GROUPS = {
    'Gunnery': ['Gunnery', 'Small Hybrid Turret', 'Small Energy Turret', 'Small Projectile Turret', 'Sharpshooter'],
    'Missiles': ['Missile Launcher Operation', 'Light Missiles', 'Rockets'],
    'Spaceship Command': ['Spaceship Command', 'Amarr Frigate', 'Caldari Frigate', 'Gallente Frigate', 'Minmatar Frigate'],
    'Navigation': ['Navigation', 'Evasive Maneuvering', 'Warp Drive Operation'],
    'Electronics': ['Electronics', 'Targeting', 'Survey'],
    'Engineering': ['Engineering', 'Energy Management', 'Shield Operation'],
    'Mechanic': ['Mechanic', 'Repair Systems', 'Hull Upgrades'],
    'Production': ['Mining', 'Refining', 'Production Efficiency'],
    'Research': ['Laboratory Operation', 'Metallurgy'],
    'Trade': ['Trade', 'Broker Relations', 'Retail'],
    'Social': ['Social', 'Negotiation', 'Diplomacy'],
    'Leadership': ['Leadership'],
}
skill = {}
for g, names in SKILL_GROUPS.items():
    group(g, 'Skill')
    for n in names:
        skill[n] = typ(n, g, graphicID=icon(25, 3))  # the skill book
for required in ('Industry', 'Science', 'CorporationManagement', 'Duplicating', 'ReverseEngineering'):
    skill[words(required)] = type_ids[required]

# ======================================================== corps and stations
# REAL corporation IDs: ui/uix.py `tempcorppaths` hardcodes every NPC
# corporation's ID -> its logo ('sheet_quadrant' of res:/UI/Texture/Corps/
# corpsNN.dds, drawn by MapCorpLogo). The names below are read off those
# logos (each logo spells its corporation's name). 1000002..1000005 ->
# CBD Corporation, Prompt Delivery, Ytiri, Hyasyoda confirms the reading.
LOGO_NAMES = {
    '1_1': 'Caldari Steel', '1_2': 'Internal Security', '1_3': 'Prompt Delivery', '1_4': 'Hyasyoda Corporation',
    '2_1': 'Lai Dai Corporation', '2_2': 'Lai Dai Protection Service', '2_3': 'Zero-G Research Firm',
    '2_4': 'Nugoeihuvi Corporation',
    '3_1': 'Kaalakiota Corporation', '3_2': 'InterBus', '3_3': 'Caldari Provisions',
    '3_4': 'Secure Commerce Commission',
    '4_1': 'DED', '4_2': 'State and Region Bank', '4_3': 'House of Records', '4_4': 'Deep Core Mining Inc.',
    '5_1': 'Modern Finances', '5_2': 'Propel Dynamics', '5_3': 'Echelon Entertainment', '5_4': 'Zainou Biotech',
    '6_1': 'Ishukone Corporation', '6_2': 'Wiyrkomi Corporation', '6_3': 'Ishukone Watch',
    '6_4': 'Wiyrkomi Peace Corps',
    '7_1': 'Mercantile Club', '7_2': 'Perkone', '7_3': 'CBD Sell Division', '7_4': 'CBD Corporation',
    '8_1': 'Ytiri', '8_2': 'Chief Executive Panel', '8_3': 'Caldari Business Tribunal', '8_4': 'Caldari Constructions',
    '9_1': 'Expert Distribution', '9_2': 'Expert Housing', '9_3': 'Caldari Funds Unlimited', '9_4': 'Rapid Assembly',
    '10_1': 'School of Applied Knowledge', '10_2': 'Science and Trade Institute', '10_3': 'Sukuuvestaa Corporation',
    '10_4': 'Top Down',
    '11_1': 'Royal Khanid Navy', '11_2': 'Khanid Transport', '11_3': 'Khanid Works', '11_4': 'Khanid Innovation',
    '12_1': 'Emperor Family', '12_2': 'Kor-Azor Family', '12_3': 'Sarum Family', '12_4': 'Ardishapur Family',
    '13_1': 'Kador Family', '13_2': 'Amarr Certified News', '13_3': 'Further Foodstuffs', '13_4': 'Civic Court',
    '14_1': 'Sisters of EVE', '14_2': 'Imperial Armaments', '14_3': 'Imperial Constructions',
    '14_4': 'Amarr Civil Service',
    '15_1': 'Imperial Trade Registry', '15_2': 'Carthum Conglomerate', '15_3': 'Tash-Murkon Family',
    '15_4': 'Joint Harvesting',
    '16_1': 'Zoar and Sons', '16_2': 'Imperial Shipment', '16_3': 'Ministry of Internal Order', '16_4': 'Imperial Navy',
    '17_1': 'Ministry of War', '17_2': 'Ducia Foundry', '17_3': 'Nurtura', '17_4': 'Inherent Implants',
    '18_1': 'Court Chamberlain', '18_2': 'Ministry of Assessment', '18_3': 'Noble Appliances',
    '18_4': 'Imperial Chancellor',
    '19_1': 'HZO Refinery', '19_2': 'Theology Council', '19_3': 'Blood Raiders', '19_4': 'Viziam',
    '20_1': 'Krusual Tribe', '20_2': 'Brutor Tribe', '20_3': 'Sebiestor Tribe', '20_4': 'Vherokior Tribe',
    '21_1': 'The Leisure Group', '21_2': 'Minmatar Mining Corporation', '21_3': 'Boundless Creation',
    '21_4': 'Six Kin Development',
    '22_1': 'Republic Justice Department', '22_2': 'Republic Fleet', '22_3': 'Native Freshfood',
    '22_4': 'Republic Security Services',
    '23_1': 'Urban Management', '23_2': 'Core Complexion Inc.', '23_3': 'Eifyr and Co.', '23_4': 'Freedom Extension',
    '24_1': 'Salvation Angels', '24_2': 'Guardian Angels', '24_3': 'Archangels', '24_4': 'Dominations',
    '25_1': 'Garoun Investment Bank', '25_2': 'Poteque Pharmaceuticals', '25_3': 'Egonics Inc.',
    '25_4': 'Allotek Industries',
    '26_1': 'Inner Zone Shipping', '26_2': 'Intaki Bank', '26_3': 'CONCORD', '26_4': 'Inner Circle',
    '27_1': 'Intaki Commerce', '27_2': 'Intaki Syndicate', '27_3': 'Combined Harvest', '27_4': 'ORE',
    '28_1': 'Intaki Space Police', '28_2': 'Astral Mining', '28_3': 'Guristas', '28_4': 'Guristas Production',
    '29_1': 'Supreme Court', '29_2': 'Federal Administration', '29_3': 'Federation Navy', '29_4': 'Roden Shipyards',
    '30_1': 'Aliastra', '30_2': 'President', '30_3': 'Federal Intelligence Office', '30_4': 'Senate',
    '31_1': 'Federation Customs', '31_2': 'The Scope', '31_3': 'Material Acquisition', '31_4': 'Bank of Luminaire',
    '32_1': 'Chemal Tech', '32_2': 'Pend Insurance', '32_3': 'CreoDron', '32_4': 'Duvolle Laboratories',
    '33_1': 'Transstellar Shipping', '33_2': 'FedMart', '33_3': 'Federal Freight', '33_4': 'Serpentis Inquest',
    '34_1': 'True Power', '34_2': "Mordu's Legion", '34_3': 'Quafe Company', '34_4': 'True Creations',
    '35_1': 'Nefantar Miner Association', '35_2': 'Ammatar Fleet', '35_3': 'Ammatar Consulate', '35_4': 'Food Relief',
    '36_1': 'Thukker Mix', '36_2': 'Society of Conscious Thought', '36_3': 'The Sanctuary', '36_4': 'Trust Partners',
    '37_1': 'Peace and Order Unit', '37_2': 'Home Guard', '37_3': 'Caldari Navy', '37_4': 'Corporate Police Force',
    '38_1': 'Serpentis Corporation', '38_2': 'Impetus', '38_3': 'Poksu Mineral Group', '38_4': 'Spacelane Patrol',
    '39_1': 'University of Caille', '39_2': 'Minedrill', '39_3': 'Jovian Directorate', '39_4': 'Royal Amarr Institute',
    '40_1': 'Shapeset', '40_2': 'Genolution', '40_3': 'Jove Navy', '40_4': 'Impro',
    '41_1': 'Prosper', '41_2': 'X-Sence', '41_3': 'Republic Parliament', '41_4': 'Center for Advanced Studies',
    '42_1': 'State War Academy', '42_3': 'Imperial Academy', '42_4': 'Federal Navy Academy',
    '43_1': 'Hedion University', '43_2': 'Republic Military School', '43_3': 'Republic University',
    '43_4': 'Pator Tech School', '44_1': 'Polaris',
}


def tempcorppaths():
    """ui/uix.py's {corporationID: 'sheet_quadrant'} literal, as the client has it."""
    import ast
    src = open(os.path.join(ROOT, 'decompiled', 'src', 'ui', 'uix.py'), encoding='utf-8').read()
    start = src.index('tempcorppaths = ') + len('tempcorppaths = ')
    return ast.literal_eval(src[start:src.index('\n', start)])


# TEMPLATE DEFAULT: which faction each corporation serves. The ID blocks
# follow the logo art (1000002-1000045 Caldari, 1000046-1000062 Minmatar,
# 1000063-1000093 Amarr, 1000094-1000120 Gallente); later IDs are listed.
FACTION_BLOCKS = [(1000002, 1000045, 500001), (1000046, 1000062, 500002), (1000063, 1000093, 500003),
                  (1000094, 1000120, 500004)]
FACTION_OF = {
    1000122: 500004, 1000123: 500007, 1000124: 500011, 1000126: 500007, 1000127: 500010, 1000129: 500014,
    1000133: 500011, 1000136: 500011, 1000138: 500011, 1000140: 500005, 1000141: 500010, 1000142: 500005,
    1000144: 500004, 1000145: 500004, 1000146: 500004, 1000147: 500009, 1000149: 500005, 1000150: 500005,
    1000151: 500008, 1000152: 500008, 1000153: 500008, 1000154: 500007, 1000155: 500005, 1000156: 500008,
    1000158: 500005, 1000160: 500015, 1000164: 500005, 1000165: 500003, 1000166: 500003, 1000167: 500001,
    1000168: 500004, 1000169: 500004, 1000170: 500002, 1000171: 500002, 1000172: 500002,
}


def faction_of(cid):
    for lo, hi, f in FACTION_BLOCKS:
        if lo <= cid <= hi:
            return f
    return FACTION_OF.get(cid)


CORP_TYPE = type_ids['Corporation']
corps = []
corp_index = {}
seen_logo = set()
for cid, logo in sorted(tempcorppaths().items()):
    if logo not in LOGO_NAMES or logo in seen_logo:
        # Two IDs share a logo (10_3, 33_1): the logo names only the first.
        continue
    seen_logo.add(logo)
    name = LOGO_NAMES[logo]
    asset(f'res:/UI/Texture/Corps/corps{int(logo.split("_")[0]):02d}.dds')
    ticker = ''.join(w[0] for w in name.replace('-', ' ').split() if w[0].isalnum())[:4].upper()
    corp_index[cid] = len(corps)
    corps.append([cid, name, ticker or name[:3].upper(), faction_of(cid), '', None])
corp_by_name = {c[1]: c[0] for c in corps}

# Station owners per faction: its corporations, one picked per system.
faction_corps = {}
for c in corps:
    if c[3] is not None:
        faction_corps.setdefault(c[3], []).append(c[0])
for f in FACTIONS:
    faction_corps.setdefault(f, faction_corps[500001])


def rng_for(*key):
    h = hashlib.sha256(repr(key).encode()).digest()
    return random.Random(int.from_bytes(h[:8], 'little'))


AU = 149_597_870_700.0
ROMAN = ['I', 'II', 'III', 'IV', 'V', 'VI', 'VII', 'VIII', 'IX', 'X', 'XI', 'XII']
celestials, stargates, stations, locations = [], {}, [], []
ids = {'sun': 40000001, 'gate': 50000001, 'station': 60000001}
gate_of = {}  # (from system, to system) -> stargate itemID


def take(kind):
    v = ids[kind]
    ids[kind] += 1
    return v


def race_of_system(sid):
    f = systems[sid]['factionID'] or regions[region_of[constellation_of[sid]]]['factionID']
    return FACTIONS[f][1] if f else 'Caldari'


def place(rnd, dist):
    a = rnd.uniform(0, 2 * math.pi)
    return dist * math.cos(a), dist * rnd.uniform(-0.02, 0.02), dist * math.sin(a)


for sid in sorted(systems):
    so = systems[sid]
    cid, rid = constellation_of[sid], region_of[constellation_of[sid]]
    rnd = rng_for('system', sid)
    name = so['itemName']
    base = dict(solarSystemID=sid, constellationID=cid, regionID=rid)
    sun = take('sun')
    lum = so['luminosity'] or 0.1
    sun_type = sun_types[min(len(sun_types) - 1, int(rnd.random() * len(sun_types)))]
    celestials.append(dict(itemID=sun, typeID=sun_type, groupID=group_ids['Sun'], orbitID=None,
                           x=0.0, y=0.0, z=0.0, radius=rnd.uniform(1.5e8, 9e8) * (0.6 + lum),
                           itemName=f'{name} - Star', celestialIndex=None, orbitIndex=None, **base))
    planets = []
    orbit = rnd.uniform(0.3, 0.6) * AU
    for i in range(rnd.randint(3, 9)):
        pid = take('sun')
        pt = planet_types[rnd.randrange(3)]
        x, y, z = place(rnd, orbit)
        pname = f'{name} {ROMAN[i]}'
        celestials.append(dict(itemID=pid, typeID=pt, groupID=group_ids['Planet'], orbitID=sun, x=x, y=y, z=z,
                               radius=rnd.uniform(2.5e6, 7.0e7) if pt == planet_types[0] else rnd.uniform(1.5e6, 9e6),
                               itemName=pname, celestialIndex=i + 1, orbitIndex=None, **base))
        planets.append((pid, pname, x, y, z))
        for m in range(rnd.randint(0, 3)):
            mx, my, mz = place(rnd, rnd.uniform(1.5e8, 6e8))
            celestials.append(dict(itemID=take('sun'), typeID=type_ids['Moon'], groupID=group_ids['Moon'],
                                   orbitID=pid, x=x + mx, y=y + my, z=z + mz, radius=rnd.uniform(4e5, 2.5e6),
                                   itemName=f'{pname} - Moon {m + 1}', celestialIndex=i + 1, orbitIndex=m + 1, **base))
        if rnd.random() < 0.3:
            bx, by, bz = place(rnd, rnd.uniform(2e8, 8e8))
            celestials.append(dict(itemID=take('sun'), typeID=type_ids['Asteroid Belt'],
                                   groupID=group_ids['AsteroidBelt'], orbitID=pid, x=x + bx, y=y + by, z=z + bz,
                                   radius=15000.0, itemName=f'{pname} - Asteroid Belt 1',
                                   celestialIndex=i + 1, orbitIndex=None, **base))
        orbit *= rnd.uniform(1.4, 1.9)
    # One gate per neighbour, out past the planets in the neighbour's direction
    # (galactic coordinates), so jumps point the way the map does.
    for to in sorted(neighbours.get(sid, [])):
        dx, dy, dz = (systems[to][c] - so[c] for c in 'xyz')
        n = math.sqrt(dx * dx + dy * dy + dz * dz) or 1.0
        dist = orbit * rnd.uniform(0.5, 1.1)
        gid = take('gate')
        gate_of[(sid, to)] = gid
        celestials.append(dict(itemID=gid, typeID=gate_types.get(race_of_system(sid), gate_types['Caldari']),
                               groupID=group_ids['Stargate'], orbitID=None,
                               x=dx / n * dist, y=dy / n * dist * 0.1, z=dz / n * dist,
                               radius=GATE_RADIUS.get(race_of_system(sid), GATE_RADIUS['Caldari']),
                               itemName=f'Stargate ({systems[to]["itemName"]})',
                               celestialIndex=None, orbitIndex=None, **base))
    # Empire space gets a station in every faction system.
    if so['factionID'] and planets:
        pid, pname, x, y, z = planets[rnd.randrange(len(planets))]
        race = race_of_system(sid)
        stid = take('station')
        owners = faction_corps[so['factionID']]
        owner = owners[rnd.randrange(len(owners))]
        sx, sy, sz = place(rnd, rnd.uniform(2e7, 9e7))
        sname = f'{pname} - {corps[corp_index[owner]][1]} Station'
        stations.append((stid, station_types[race], owner, sid, cid, rid, sname, 0, x + sx, y + sy, z + sz))
        celestials.append(dict(itemID=stid, typeID=station_types[race], groupID=group_ids['Station'], orbitID=pid,
                               x=x + sx, y=y + sy, z=z + sz, radius=STATION_RADIUS[race], itemName=sname,
                               celestialIndex=None, orbitIndex=None, **base))

for (a, b), gid in gate_of.items():
    stargates[gid] = gate_of[(b, a)]

# Every station offers every service (eve.stationItem.serviceMask).
all_services = 0
for k, v in seed.values.items():
    if k.startswith('stationService'):
        all_services |= v
stations = [s[:7] + (all_services,) + s[8:] for s in stations]
station_by_system = {s[3]: s for s in stations}

# =================================================================== dogma
# Attributes beyond the client-contract ones: names the UI reads through godma
# (fitting/ship UI: cpuOutput, powerOutput, shieldCapacity, armorHP, hp, ...),
# skill bookkeeping, and the character attributes' item values.
next_attr = [100]
extra_attrs = []
attr_id = dict((seed.lower_first(k), v) for k, v in seed.attr_id.items())


def attribute(name, display=None, category=0, unit=None, recharge=None, maximum=None):
    if name in attr_id:
        return attr_id[name]
    aid = next_attr[0]
    next_attr[0] += 1
    extra_attrs.append((aid, name, display or words(name[0].upper() + name[1:]), 0.0, 1, unit, None, category,
                        recharge, maximum))
    attr_id[name] = aid
    return aid


for n in ['cpuOutput', 'powerOutput', 'cpuLoad', 'powerLoad', 'hp', 'armorHP', 'armorDamage', 'shieldCapacity',
          'capacitorCapacity', 'maxLockedTargets', 'signatureRadius', 'radius', 'skillTimeConstant', 'skillLevel',
          'skillPoints', 'warpSpeedMultiplier', 'damage']:
    attribute(n)
# Charged attributes: value recharges towards its maximum with tau = recharge/5
# (godma CreateChargedAttribute). attributeCategory 8 is excluded by godma.
attribute('charge', category=0, recharge=attr_id['rechargeRate'], maximum=attr_id['capacitorCapacity'])
attribute('shieldCharge', category=0, recharge=attr_id['shieldRechargeRate'], maximum=attr_id['shieldCapacity'])

type_attrs = []
# TEMPLATE DEFAULT rookie-ship dogma. Radius from the model would be better
# (the client draws hulls at modelled size); these are frigate-scale figures.
ROOKIE_DOGMA = dict(hiSlots=2, medSlots=1, lowSlots=1, turretSlotsLeft=2, launcherSlotsLeft=0, mass=1100000.0,
                    maxVelocity=300.0, agility=3.2, cpuOutput=100.0, powerOutput=30.0, capacitorCapacity=250.0,
                    rechargeRate=125000.0, shieldCapacity=150.0, shieldRechargeRate=600000.0, armorHP=150.0,
                    hp=200.0, maxTargetRange=20000.0, maxLockedTargets=2, scanSpeed=1500.0, signatureRadius=40.0,
                    radius=40.0, capacity=135.0, warpSpeedMultiplier=1.0, droneCapacity=10.0)
for tid in rookie_ship.values():
    for k, v in ROOKIE_DOGMA.items():
        type_attrs.append((tid, attr_id[k], float(v)))
for k, v in dict(mass=32000.0, maxVelocity=200.0, agility=1.0, hp=100.0, shieldCapacity=50.0, armorHP=50.0,
                 capacitorCapacity=50.0, rechargeRate=60000.0, shieldRechargeRate=300000.0, radius=8.0,
                 signatureRadius=25.0, maxTargetRange=10000.0, maxLockedTargets=1, scanSpeed=2000.0,
                 warpSpeedMultiplier=1.0).items():
    type_attrs.append((type_ids['Capsule'], attr_id[k], float(v)))
# Skills: primary/secondary character attribute and rank (skillTimeConstant).
PRIMARY = {'Gunnery': ('perception', 'willpower'), 'Missiles': ('perception', 'willpower'),
           'Spaceship Command': ('perception', 'willpower'), 'Navigation': ('intelligence', 'perception'),
           'Electronics': ('intelligence', 'memory'), 'Engineering': ('intelligence', 'memory'),
           'Mechanic': ('intelligence', 'memory'), 'Production': ('memory', 'intelligence'),
           'Research': ('intelligence', 'memory'), 'Trade': ('charisma', 'memory'),
           'Social': ('charisma', 'intelligence'), 'Leadership': ('charisma', 'willpower')}
for g, names in SKILL_GROUPS.items():
    p1, p2 = PRIMARY[g]
    for n in names:
        tid = skill[n]
        type_attrs += [(tid, attr_id['primaryAttribute'], float(attr_id[p1])),
                       (tid, attr_id['secondaryAttribute'], float(attr_id[p2])),
                       (tid, attr_id['skillTimeConstant'], 1.0 if n == g else 2.0)]
for req in ('Industry', 'Science', 'CorporationManagement', 'Duplicating', 'ReverseEngineering'):
    type_attrs += [(type_ids[req], attr_id['primaryAttribute'], float(attr_id['intelligence'])),
                   (type_ids[req], attr_id['secondaryAttribute'], float(attr_id['memory'])),
                   (type_ids[req], attr_id['skillTimeConstant'], 1.0)]

# ============================================================ gear and ore
# TEMPLATE DEFAULT: what ships fit, shoot, mine and launch. Every model is a
# client asset: turret graphics are the 6-letter codes model/turret.py
# assembles (race, size, weapon type, barrel id, tech level, ammo) from
# res:/Model/Turret/Base/base_<race><size><tech>_l33t.blue and Barrel/
# barrel_<size><type>0<id>_l33t.blue; drone and asteroid names are their
# model files' names.
for n in ['gfxBoosterID', 'maxRange', 'falloff', 'trackingSpeed', 'speed', 'duration', 'capacitorNeed',
          'damageMultiplier', 'emDamage', 'thermalDamage', 'kineticDamage', 'explosiveDamage', 'miningAmount',
          'chargeGroup1', 'entityAttackRange', 'entityLootValue', 'oreQuantity']:
    attribute(n)

# Engine flares. ship.py FitHardpoints falls back to graphicID 395 when a hull
# has no gfxBoosterID, and names the thruster sound after the booster model
# (res:/Sound/Thruster/<booster>_l.blue), so 395 is the blue booster.
BOOSTER_GRAPHIC = {'blue': 395, 'green': 391, 'orange': 392, 'red': 393, 'yellow': 394}
for colour, gid in BOOSTER_GRAPHIC.items():
    graphics.append((gid, asset(f'res:/Model/Ship/Booster/booster_{colour}.blue'), None, None))
RACE_BOOSTER = {'Caldari': 'blue', 'Gallente': 'green', 'Minmatar': 'orange', 'Amarr': 'yellow'}
for race, tid in rookie_ship.items():
    type_attrs.append((tid, attr_id['gfxBoosterID'], float(BOOSTER_GRAPHIC[RACE_BOOSTER[race]])))


def turret_graphic(code):
    race, size, kind, barrel, tech = code[0], code[1], code[2], code[3], code[4]
    asset(f'res:/Model/Turret/Base/base_{race}{size}{tech}_l33t.blue')
    asset(f'res:/Model/Turret/Barrel/barrel_{size}{kind}0{barrel}_l33t.blue')
    while next_graphic[0] in RESERVED:
        next_graphic[0] += 1
    gid = next_graphic[0]
    next_graphic[0] += 1
    graphics.append((gid, code, None, None))
    return gid


# Effects: the module's default effect is what the ship UI activates
# (shipmodulebutton.IsDefaultEffectActivatible: isDefault, not 'online',
# category dgmEffActivation or dgmEffTarget). 'online' is category 4.
EFF_ACTIVATION, EFF_TARGET, EFF_ONLINE = seed.values['dgmEffActivation'], seed.values['dgmEffTarget'], 4
effect_rows = []
effect_id = {}


def effect(name, category, offensive=0, tracking=None, rng=None, falloff=None, discharge=None, duration=None):
    eid = 100 + len(effect_rows)
    effect_rows.append((eid, name, words(name[0].upper() + name[1:]), '', category, offensive,
                        tracking and attr_id[tracking], rng and attr_id[rng], falloff and attr_id[falloff],
                        discharge and attr_id[discharge], duration and attr_id[duration], None))
    effect_id[name] = eid
    return eid


effect('online', EFF_ONLINE)
effect('targetAttack', EFF_TARGET, 1, 'trackingSpeed', 'maxRange', 'falloff', 'capacitorNeed', 'speed')
effect('mining', EFF_TARGET, 0, None, 'maxRange', None, 'capacitorNeed', 'duration')
effect('shieldBoosting', EFF_ACTIVATION, 0, None, None, None, 'capacitorNeed', 'duration')
seed_effect = {seed.lower_first(e): i + 1 for i, e in enumerate(seed.effects)}
slot_effect = {'hi': seed_effect['hiPower'], 'med': seed_effect['medPower'], 'lo': seed_effect['loPower']}
type_effects = []


def module(name, group_name, slot, default_effect, graphicID, attrs, skill_req=None):
    tid = typ(name, group_name, graphicID=graphicID, volume=5.0, capacity=attrs.pop('capacity', 0.0))
    for k, v in attrs.items():
        type_attrs.append((tid, attr_id[k], float(v)))
    if skill_req:
        type_attrs.extend([(tid, attr_id['requiredSkill1'], float(skill[skill_req])),
                           (tid, attr_id['requiredSkill1Level'], 1.0)])
    type_effects.append((tid, effect_id['online'], 0))
    type_effects.append((tid, slot_effect[slot], 0))
    if default_effect:
        type_effects.append((tid, effect_id[default_effect], 1))
    return tid


group('Ammo', 'Charge')
ammo = {}
AMMO = {  # name: (group-of-weapon, em, thermal, kinetic, explosive, icon)
    'Antimatter Charge S': ('HybridWeapon', 0, 5, 7, 0, (14, 1)),
    'Iron Charge S': ('HybridWeapon', 0, 2, 3, 0, (14, 3)),
    'Phased Plasma S': ('ProjectileWeapon', 0, 6, 2, 0, (15, 1)),
    'EMP S': ('ProjectileWeapon', 6, 0, 1, 2, (15, 3)),
}
for n, (_, em, th, ki, ex, ic) in AMMO.items():
    tid = typ(n, 'Ammo', graphicID=icon(*ic), volume=0.01, portionSize=100)
    ammo[n] = tid
    for k, v in dict(emDamage=em, thermalDamage=th, kineticDamage=ki, explosiveDamage=ex, chargeSize=1).items():
        type_attrs.append((tid, attr_id[k], float(v)))
gear = {}
gear['Miner I'] = module('Miner I', 'MiningLaser', 'hi', 'mining', turret_graphic('mim11a'),
                         dict(cpu=60, power=2, maxRange=10000, duration=60000, capacitorNeed=5, miningAmount=40),
                         'Mining')
gear['Light Railgun I'] = module('Light Railgun I', 'HybridWeapon', 'hi', 'targetAttack', turret_graphic('csh11a'),
                                 dict(cpu=10, power=4, maxRange=6000, falloff=4000, trackingSpeed=0.3, speed=3000,
                                      capacitorNeed=1, damageMultiplier=2.0, chargeGroup1=group_ids['Ammo'],
                                      chargeSize=1, capacity=0.5), 'Small Hybrid Turret')
gear['Light Autocannon I'] = module('Light Autocannon I', 'ProjectileWeapon', 'hi', 'targetAttack',
                                    turret_graphic('msp11a'),
                                    dict(cpu=8, power=3, maxRange=1500, falloff=4000, trackingSpeed=0.4, speed=2500,
                                         capacitorNeed=0, damageMultiplier=2.0, chargeGroup1=group_ids['Ammo'],
                                         chargeSize=1, capacity=0.5), 'Small Projectile Turret')
# Lasers draw their power from the capacitor and use no ammunition here.
gear['Light Pulse Laser I'] = module('Light Pulse Laser I', 'EnergyWeapon', 'hi', 'targetAttack',
                                     turret_graphic('ase11a'),
                                     dict(cpu=12, power=5, maxRange=4000, falloff=2000, trackingSpeed=0.35, speed=3500,
                                          capacitorNeed=4, damageMultiplier=1.0, emDamage=6, thermalDamage=4),
                                     'Small Energy Turret')
gear['Shield Booster I'] = module('Shield Booster I', 'ShieldBooster', 'med', 'shieldBoosting', icon(2, 1),
                                  dict(cpu=20, power=5, duration=5000, capacitorNeed=20, shieldBonus=15),
                                  'Shield Operation')
RACE_GUN = {'Caldari': 'Light Railgun I', 'Gallente': 'Light Railgun I', 'Minmatar': 'Light Autocannon I',
            'Amarr': 'Light Pulse Laser I'}
RACE_AMMO = {'Caldari': 'Antimatter Charge S', 'Gallente': 'Antimatter Charge S', 'Minmatar': 'Phased Plasma S',
             'Amarr': None}

# Ores: one per asteroid model (res:/Model/WorldObject/Asteroid/<model>.blue),
# mined as items of the asteroid's own type and reprocessed into minerals.
group('Mineral', 'Trading')
MINERALS = ['Ferrite', 'Silicate', 'Crystal Alloy', 'Isotope Gel', 'Nocturne']  # TEMPLATE DEFAULT names
mineral = {m: typ(m, 'Mineral', graphicID=icon(23, 5 + i), volume=0.01, portionSize=1)
           for i, m in enumerate(MINERALS)}
ASTEROID_MODELS = sorted(p.rsplit('/', 1)[1][:-5] for p in ASSET_NAMES
                         if p.lower().startswith('res/model/worldobject/asteroid/') and p.lower().endswith('.blue'))
ore, type_materials = {}, []
for i, model_name in enumerate(ASTEROID_MODELS):
    name = model_name.replace('_', ' ').title()
    tid = typ(name, 'Asteroid', graphicID=graphic(f'res:/Model/WorldObject/Asteroid/{model_name}.blue'),
              volume=0.1 * (1 + i % 4), portionSize=100)
    ore[name] = tid
    type_attrs.append((tid, attr_id['radius'], 800.0 + 150.0 * (i % 5)))
    # Each 100 units refine into a few minerals, richer ores giving rarer ones.
    for j, m in enumerate(MINERALS):
        qty = [400, 0, 0, 0, 0][j] if i % 5 == 0 else max(0, (5 - abs(j - i % 5)) * 40 - j * 25)
        if qty:
            type_materials.append((tid, mineral[m], qty))

# Drones: combat drones are named after their models (res:/Model/Drone/<name><n>.blue).
group('CombatDrone', 'Drone')
group('MiningDrone', 'Drone')
DRONE_MODELS = ['Acolyte', 'Hornet', 'Hobgoblin', 'Warrior', 'Wasp', 'Hammerhead', 'Ogre', 'Berserker',
                'Praeator', 'Wisp']
drones = {}
for i, d in enumerate(DRONE_MODELS):
    name = f'{d} I'
    tid = typ(name, 'CombatDrone', graphicID=graphic(f'res:/Model/Drone/{d}1.blue'), volume=5.0 * (1 + i // 4))
    drones[name] = tid
    for k, v in dict(mass=3000.0, maxVelocity=900.0 - 50 * (i // 4), agility=0.5, radius=10.0, hp=60 * (1 + i // 4),
                     armorHP=40.0, shieldCapacity=40.0, maxRange=1500.0, falloff=1000.0, trackingSpeed=1.5,
                     speed=4000.0, damageMultiplier=1.0 + i // 4, kineticDamage=4.0, thermalDamage=2.0).items():
        type_attrs.append((tid, attr_id[k], float(v)))
    type_effects.append((tid, effect_id['targetAttack'], 1))
drones['Mining Drone I'] = tid = typ('Mining Drone I', 'MiningDrone',
                                    graphicID=graphic('res:/Model/Drone/drone_n/Drone_01.blue'), volume=5.0)
for k, v in dict(mass=3000.0, maxVelocity=500.0, agility=0.5, radius=10.0, hp=60.0, maxRange=1000.0,
                 duration=60000.0, miningAmount=15.0).items():
    type_attrs.append((tid, attr_id[k], float(v)))
type_effects.append((tid, effect_id['mining'], 1))

# Belt pirates (groupPirateDrone, categoryEntity) on the shipped pirate hulls:
# they lock and shoot ships in range and pay a bounty (entityKillBounty).
group('PirateDrone', 'Entity')
PIRATES = {'Pirate Scout': ('pirf1/pirf1', 1), 'Pirate Raider': ('pirf6/pf6', 2), 'Pirate Marauder': ('pirc1/pirc1', 3)}
pirates = {}
for n, (model, tier) in PIRATES.items():
    tid = typ(n, 'PirateDrone', graphicID=graphic(f'res:/Model/Ship/pirate/{model}.blue'), volume=27000.0 * tier)
    pirates[n] = tid
    for k, v in dict(mass=1.0e6 * tier, maxVelocity=350.0 - 60 * tier, agility=2.0, radius=40.0 * tier,
                     hp=150.0 * tier, armorHP=120.0 * tier, shieldCapacity=100.0 * tier, maxRange=4000.0,
                     falloff=3000.0, trackingSpeed=0.3, speed=4000.0, damageMultiplier=1.0 * tier,
                     kineticDamage=4.0, thermalDamage=3.0, entityAttackRange=20000.0,
                     entityKillBounty=2500.0 * tier * tier, entityLootValue=tier, signatureRadius=40.0 * tier,
                     gfxBoosterID=float(BOOSTER_GRAPHIC['red'])).items():
        type_attrs.append((tid, attr_id[k], float(v)))
    type_effects.append((tid, effect_id['targetAttack'], 1))

# The character as a dogma item: how many targets a pilot can hold.
type_attrs.append((type_ids['Character'], attr_id['maxLockedTargets'], 2.0))

# ================================================================== market
# TEMPLATE DEFAULT: the market tree (market.GetMarketGroups) and base prices
# the stations' standing NPC orders trade at. A type is listed when its
# marketGroupID is set (cfg.invtypes ... .marketGroupID, marketquote.py).
market_groups = []


def market_group(name, parent=None, icon_graphic=None, has_types=0):
    mid = len(market_groups) + 1
    market_groups.append((mid, parent, name, '', icon_graphic, has_types))
    return mid


type_price = {}


def list_on_market(type_id, group_id, price):
    for t in types:
        if t['typeID'] == type_id:
            t['marketGroupID'] = group_id
    type_price[type_id] = float(price)


mg_equipment = market_group('Ship Equipment', icon_graphic=icon(12, 8))
mg_turrets = market_group('Turrets', mg_equipment, icon(12, 9), 1)
mg_mining = market_group('Mining Equipment', mg_equipment, icon(12, 9), 1)
mg_shields = market_group('Shield Boosters', mg_equipment, icon(2, 1), 1)
mg_ammo = market_group('Ammunition', icon_graphic=icon(14, 1), has_types=1)
mg_drones = market_group('Drones', icon_graphic=icon(26, 2), has_types=1)
mg_ships = market_group('Ships', icon_graphic=icon(9, 5), has_types=1)
mg_materials = market_group('Manufacture & Research', icon_graphic=icon(24, 1))
mg_minerals = market_group('Minerals', mg_materials, icon(23, 5), 1)
mg_ore = market_group('Ore', mg_materials, icon(24, 1), 1)
mg_skills = market_group('Skills', icon_graphic=icon(25, 3))
for name in ('Light Railgun I', 'Light Autocannon I', 'Light Pulse Laser I'):
    list_on_market(gear[name], mg_turrets, 25000)
list_on_market(gear['Miner I'], mg_mining, 10000)
list_on_market(gear['Shield Booster I'], mg_shields, 30000)
for n, tid in ammo.items():
    list_on_market(tid, mg_ammo, 8)
for i, (n, tid) in enumerate(drones.items()):
    list_on_market(tid, mg_drones, 15000 + 5000 * (i // 4))
for tid in rookie_ship.values():
    list_on_market(tid, mg_ships, 25000)
for i, (n, tid) in enumerate(mineral.items()):
    list_on_market(tid, mg_minerals, [2, 6, 40, 120, 600][i])
for i, (n, tid) in enumerate(ore.items()):
    list_on_market(tid, mg_ore, 10 + 4 * (i % 5))
for g, names in SKILL_GROUPS.items():
    sub = market_group(g, mg_skills, icon(25, 3), 1)
    for n in names:
        list_on_market(skill[n], sub, 20000 if n == g else 60000)

# ================================================================ industry
# TEMPLATE DEFAULT: the minerals one portion (portionSize units) of each
# manufactured type is built from (factory.GetMineralCompositionOfItemType;
# recycling at a reprocessing plant gives part of them back), and one
# blueprint per product (cfg.invbptypes, factory.GetBlueprintAttributes).
group('Blueprint', 'Blueprint')
BUILD_BASE = {  # mineral: quantity per portion, by kind of product
    'module': {'Ferrite': 800, 'Silicate': 300, 'Crystal Alloy': 60},
    'ammo': {'Ferrite': 60, 'Silicate': 20},
    'drone': {'Ferrite': 1500, 'Silicate': 400, 'Crystal Alloy': 100, 'Isotope Gel': 10},
    'ship': {'Ferrite': 6000, 'Silicate': 2000, 'Crystal Alloy': 300, 'Isotope Gel': 40, 'Nocturne': 4},
}
# kind: (manufacturingTime s, wastageFactor, research time s, duplicate chance)
BUILD_KIND = {'module': (1800, 0.1, 3600, 0.2), 'ammo': (600, 0.1, 1800, 0.3),
              'drone': (2400, 0.1, 4800, 0.15), 'ship': (7200, 0.1, 14400, 0.05)}
type_name = {t['typeID']: t['typeName'] for t in types}
products = ([(tid, 'module') for tid in gear.values()] + [(tid, 'ammo') for tid in ammo.values()]
            + [(tid, 'drone') for tid in drones.values()] + [(tid, 'ship') for tid in rookie_ship.values()])
bptypes, blueprint_info = [], []
mg_blueprints = market_group('Blueprints', icon_graphic=icon(24, 1), has_types=1)
for i, (tid, kind) in enumerate(products):
    for m, n in BUILD_BASE[kind].items():
        type_materials.append((tid, mineral[m], n + n * (i % 3) // 5))
    t = next(t for t in types if t['typeID'] == tid)
    t['chanceOfDuplicating'] = BUILD_KIND[kind][3]
    bp_name = f'{type_name[tid]} Blueprint'
    bp = typ(bp_name, 'Blueprint', graphicID=t.get('graphicID'), volume=0.01)
    price = 10 * type_price.get(tid, 1000.0)
    bptypes.append((bp, bp_name, f'Manufactures {type_name[tid]}.', None, tid, 1, 0.1, price))
    build_time, waste, research, _ = BUILD_KIND[kind]
    blueprint_info.append((bp, build_time, waste, research, research // 2, research))
    list_on_market(bp, mg_blueprints, price)

# ============================================================ creation content
# TEMPLATE DEFAULT. Races/bloodlines extend the client-contract rows by name.
races_text = {
    'Caldari': 'The Caldari State is a corporate dictatorship, run by megacorporations that own everything.',
    'Minmatar': 'The Minmatar Republic, free once more, is a proud people rebuilding after centuries of slavery.',
    'Amarr': 'The Amarr Empire is the largest and oldest of the empires, ruled by an Emperor and a strict faith.',
    'Gallente': 'The Gallente Federation is a democracy that values liberty and individual freedom above all.',
    'Jove': 'The Jove are an enigma: technologically far ahead, they rarely deal with the other empires.',
}
race_skill = {'Caldari': 'Caldari Frigate', 'Minmatar': 'Minmatar Frigate', 'Amarr': 'Amarr Frigate',
              'Gallente': 'Gallente Frigate', 'Jove': 'Spaceship Command'}
# Race emblems: icons19 is a 2x2 sheet of the four empire logos (icon19_01..04);
# the Jove (not playable, see face.py) get the Jovian Directorate corp logo.
RACE_LOGO = {'Caldari': lambda: icon(19, 1), 'Minmatar': lambda: icon(19, 2), 'Gallente': lambda: icon(19, 3),
             'Amarr': lambda: icon(19, 4), 'Jove': lambda: corp_logo(39, 3)}
race_rows = []
for race, rid in RACE.items():
    race_rows.append((rid, None, RACE_LOGO[race](), skill[race_skill[race]]))
out.append('\n'.join(
    f'UPDATE static.races SET "description" = {q(races_text[r])}, "graphicID" = {g}, "skillTypeID1" = {s} '
    f'WHERE "raceID" = {RACE[r]};' for r, (_, _, g, s) in zip(RACE, race_rows)) + '\n')

# Base attributes per bloodline (charisma, intelligence, memory, perception, willpower).
BLOODLINES = {
    'Deteis': (5, 11, 10, 8, 6, 'Caldari'), 'Civire': (6, 8, 7, 11, 8, 'Caldari'),
    'Sebiestor': (6, 11, 8, 8, 7, 'Minmatar'), 'Brutor': (6, 6, 8, 10, 10, 'Minmatar'),
    'Amarr': (7, 7, 8, 8, 10, 'Amarr'), 'Ni Kunni': (9, 7, 9, 7, 8, 'Amarr'),
    'Gallente': (10, 8, 7, 8, 7, 'Gallente'), 'Intaki': (8, 9, 9, 7, 7, 'Gallente'),
}
BL_SKILLS = {
    'Deteis': ('Electronics', 'Science'), 'Civire': ('Gunnery', 'Navigation'),
    'Sebiestor': ('Engineering', 'Mechanic'), 'Brutor': ('Gunnery', 'Mechanic'),
    'Amarr': ('Leadership', 'Engineering'), 'Ni Kunni': ('Trade', 'Social'),
    'Gallente': ('Social', 'Navigation'), 'Intaki': ('Science', 'Diplomacy'),
}
# Bloodline emblems: icons28 = Amarr, Gallente, Intaki, Civire; icons29 = Ni-Kunni,
# Deteis, Sebiestor, Brutor (2x2 sheets, named in the art).
BLOODLINE_LOGO = {'Amarr': (28, 1), 'Gallente': (28, 2), 'Intaki': (28, 3), 'Civire': (28, 4),
                  'Ni Kunni': (29, 1), 'Deteis': (29, 2), 'Sebiestor': (29, 3), 'Brutor': (29, 4)}
for i, (bl, (ch, it, me, pe, wi, race)) in enumerate(BLOODLINES.items()):
    s1, s2 = BL_SKILLS[bl]
    out.append(
        f'UPDATE static.bloodlines SET "description" = {q(f"The {bl} bloodline of the {race} people.")}, '
        f'"maleDescription" = {q(f"A {bl} man.")}, "femaleDescription" = {q(f"A {bl} woman.")}, '
        f'"graphicID" = {icon(*BLOODLINE_LOGO[bl])}, "skillTypeID1" = {skill[s1]}, "skillTypeID2" = {skill[s2]}, '
        f'"charisma" = {ch}, "intelligence" = {it}, "memory" = {me}, "perception" = {pe}, "willpower" = {wi}, '
        f'"shipTypeID" = {rookie_ship[race]} WHERE "bloodlineName" = {q(bl)};')
out.append('')

ATTRS = ['Charisma', 'Intelligence', 'Memory', 'Perception', 'Willpower']
ATTR_TEXT = {
    'Charisma': 'Your ability to communicate and lead.', 'Intelligence': 'Your ability to learn and reason.',
    'Memory': 'Your ability to recall information.', 'Perception': 'Your awareness of your surroundings.',
    'Willpower': 'Your mental strength and resolve.'}
emit('cc_attributes', ['attributeID', 'attributeName', 'description', 'graphicID'],
     [(seed.values['chrattr' + a], a, ATTR_TEXT[a], icon(22, 1 + i)) for i, a in enumerate(ATTRS)])

ANCESTRY = ['Merchants', 'Mercenaries', 'Scholars']
ANCESTRY_ICON = [(30, 13), (31, 4), (30, 12)]  # money, mercenary emblem, microscope
anc_rows = []
for b_i, bl in enumerate(BLOODLINES):
    blid = b_i + 1
    for a_i, a in enumerate(ANCESTRY):
        post = [0] * 5
        post[(b_i + a_i) % 5] = 2
        post[(b_i + a_i + 2) % 5] = 1
        s = [('Trade', 'Negotiation'), ('Gunnery', 'Evasive Maneuvering'), ('Science', 'Laboratory Operation')][a_i]
        anc_rows.append((len(anc_rows) + 1, f'{words(bl)} {a}', blid,
                         f'Your family were {a.lower()} among the {bl}.', icon(*ANCESTRY_ICON[a_i]), *post,
                         skill[s[0]], skill[s[1]]))
emit('cc_ancestries', ['ancestryID', 'ancestryName', 'bloodlineID', 'description', 'graphicID',
                       'postCharisma', 'postIntelligence', 'postMemory', 'postPerception', 'postWillpower',
                       'skillTypeID1', 'skillTypeID2'], anc_rows)

# Schools: three per playable race, each an NPC corporation in the race's home region.
HOME_REGION = {'Caldari': 'The Forge', 'Minmatar': 'Heimatar', 'Amarr': 'Domain', 'Gallente': 'Sinq Laison'}
SCHOOLS = {
    'Caldari': ['State War Academy', 'Science and Trade Institute', 'School of Applied Knowledge'],
    'Minmatar': ['Republic Military School', 'Republic University', 'Pator Tech School'],
    'Amarr': ['Imperial Academy', 'Hedion University', 'Royal Amarr Institute'],
    'Gallente': ['Federal Navy Academy', 'University of Caille', 'Center for Advanced Studies'],
}
# Each school's own logo ships in the corp sheets (named in the art).
SCHOOL_LOGO = {
    'State War Academy': (42, 1), 'Science and Trade Institute': (10, 2), 'School of Applied Knowledge': (10, 1),
    'Republic Military School': (43, 2), 'Republic University': (42, 2), 'Pator Tech School': (43, 4),
    'Imperial Academy': (42, 3), 'Hedion University': (43, 1), 'Royal Amarr Institute': (39, 4),
    'Federal Navy Academy': (42, 4), 'University of Caille': (39, 1), 'Center for Advanced Studies': (41, 4),
}
CAREERS = {  # department -> field -> (skills, specialities)
    'Military': {'Gunnery': (('Small Hybrid Turret', 'Sharpshooter'), ('Marksman', 'Gunner')),
                 'Missiles': (('Light Missiles', 'Rockets'), ('Missileer', 'Bombardier'))},
    'Industry': {'Mining': (('Mining', 'Refining'), ('Miner', 'Refiner')),
                 'Production': (('Production Efficiency', 'Metallurgy'), ('Manufacturer', 'Researcher'))},
    'Commerce': {'Trade': (('Trade', 'Retail'), ('Trader', 'Broker')),
                 'Diplomacy': (('Negotiation', 'Diplomacy'), ('Negotiator', 'Diplomat'))},
}
DEPT_SKILLS = {'Military': ('Gunnery', 'Missile Launcher Operation', 'Targeting'),
               'Industry': ('Mining', 'Science', 'Industry'),
               'Commerce': ('Trade', 'Social', 'Broker Relations')}
# Career pictures: icons17/32/33 are 2x2 sheets of illustrations, the rest 4x4 item icons.
DEPT_ICON = {'Military': (32, 1), 'Industry': (17, 4), 'Commerce': (17, 2)}
FIELD_ICON = {'Gunnery': (12, 9), 'Missiles': (13, 4), 'Mining': (32, 2), 'Production': (17, 1),
              'Trade': (33, 1), 'Diplomacy': (25, 9)}
schools, depts, fields, specs = [], [], [], []
for race, names in SCHOOLS.items():
    home = [sid for sid in systems if region_of[constellation_of[sid]] ==
            next(r for r, o in regions.items() if o['itemName'] == HOME_REGION[race]) and sid in station_by_system]
    home.sort(key=lambda s: -(systems[s]['security'] or 0))
    for i, sname in enumerate(names):
        st = station_by_system[home[i % len(home)]]
        cid = corp_by_name[sname]
        corps[corp_index[cid]][5] = st[0]
        schools.append((len(schools) + 1, RACE[race], sname, f'{sname} trains capsuleers of the {race} people.',
                        corp_logo(*SCHOOL_LOGO[sname]), cid, st[0]))
        for d_i, (dname, dfields) in enumerate(CAREERS.items()):
            did = len(depts) + 1
            d_sk = [skill[s] for s in DEPT_SKILLS[dname]]
            depts.append((did, schools[-1][0], dname, f'The {dname.lower()} department of the {sname}.',
                          icon(*DEPT_ICON[dname]), *d_sk))
            for fname, (fsk, fspecs) in dfields.items():
                fid = len(fields) + 1
                fields.append((fid, did, fname, f'Training in {fname.lower()}.', icon(*FIELD_ICON[fname]),
                               skill[fsk[0]], skill[fsk[1]], None, None))
                for sp in fspecs:
                    specs.append((len(specs) + 1, fid, sp, f'Specialise as a {sp.lower()}.', icon(*FIELD_ICON[fname]),
                                  skill[fsk[1]], None, None, None))
emit('cc_schools', ['schoolID', 'raceID', 'schoolName', 'description', 'graphicID', 'corporationID', 'stationID'],
     schools)
emit('cc_departments', ['departmentID', 'schoolID', 'departmentName', 'description', 'graphicID',
                        'skillTypeID1', 'skillTypeID2', 'skillTypeID3'], depts)
emit('cc_fields', ['fieldID', 'departmentID', 'fieldName', 'description', 'graphicID', 'skillTypeID1',
                   'skillTypeID2', 'typeID', 'typeQuantity'], fields)
emit('cc_specialities', ['specialityID', 'fieldID', 'specialityName', 'description', 'graphicID',
                         'skillTypeID1', 'skillTypeID2', 'typeID', 'typeQuantity'], specs)

# Default corporation per bloodline = the race's first school.
for b_i, (bl, v) in enumerate(BLOODLINES.items()):
    corp_id = next(s[5] for s in schools if s[1] == RACE[v[5]])
    out.append(f'UPDATE static.bloodlines SET "corporationID" = {corp_id} WHERE "bloodlineName" = {q(bl)};')
out.append('')

# Appearance options from facetree.txt, only where the model exists.
FAMILY = {'skin': 'skins', 'eyes': 'eyes', 'eyebrows': 'eyebrows', 'makeup': 'makeups', 'lipstick': 'lipsticks',
          'deco': 'decos', 'hair': 'hairs', 'beard': 'beards', 'accessories': 'accessories', 'costume': 'costumes'}
BL_DIR = {('Amarr', 'Amarr'): 'Amarr', ('Amarr', 'Nikunni'): 'Ni Kunni', ('Caldari', 'Civire'): 'Civire',
          ('Caldari', 'Deteis'): 'Deteis', ('Gallente', 'Gallente'): 'Gallente', ('Gallente', 'Intaki'): 'Intaki',
          ('Minmatar', 'Brutor'): 'Brutor', ('Minmatar', 'Sebiestor'): 'Sebiestor'}
bl_id = {bl: i + 1 for i, bl in enumerate(BLOODLINES)}
names_by_family, options, stack = {}, set(), []
for line in open(os.path.join(ROOT, 'client_res', 'res', 'ui', 'Login', 'facetree.txt'), encoding='latin-1'):
    if not line.strip():
        continue
    depth = (len(line) - len(line.lstrip(' '))) // 2
    stack = stack[:depth] + [line.strip()]
    if stack[0] == 'SceneOptions' and depth == 2:
        fam = {'Background': 'backgrounds', 'Lights': 'lights'}[stack[1]]
        if f'res/model/face/sceneoptions/{stack[1]}/{stack[2]}/default.blue'.lower() in ASSETS:
            names_by_family.setdefault(fam, {}).setdefault(stack[2], len(names_by_family.get(fam, {})) + 1)
    elif depth == 5 and stack[3] == 'face' and stack[4] in FAMILY:
        race, bl, gender, _, famdir, name = stack
        path = f'res/model/face/{race}/{bl}/{gender}/face/{famdir}/{name}/default.blue'.lower()
        if path not in ASSETS:
            continue
        fam = FAMILY[famdir]
        ids_ = names_by_family.setdefault(fam, {})
        oid = ids_.setdefault(name, len(ids_) + 1)
        options.add((fam, bl_id[BL_DIR[(race, bl)]], {'female': 0, 'male': 1}[gender], oid))
emit('cc_appearance_names', ['family', 'id', 'name'],
     sorted((f, i, n) for f, d in names_by_family.items() for n, i in d.items()))
emit('cc_appearance_options', ['family', 'bloodlineID', 'gender', 'id'], sorted(options))

# ====================================================== owners & locations
# Bulk preloads stay small (1 MB frame cap): factions, corps, map; everything
# else (characters, celestials, items) is fetched lazily by GetMulti*Ex.
emit('groups', ['groupID', 'categoryID', 'groupName'], groups)
emit('dgmattribs', ['attributeID', 'attributeName', 'displayName', 'defaultValue', 'published', 'unitID', 'graphicID',
                    'attributeCategory', 'chargeRechargeTimeID', 'maxAttributeID'], extra_attrs)
emit('graphics', ['graphicID', 'url3D', 'icon', 'explosionID'], graphics)
emit('logo_library', ['graphicID', 'kind', 'path', 'label'],
     [(g, 'shape', p, None) for g, p in logo_shapes] + [(g, 'color', p, l) for g, p, l in logo_colors])

# TEMPLATE DEFAULT service tables.
emit('petition_queues', ['queueID', 'queueName', 'escalatesTo'],
     [(1, 'Support', 2), (2, 'Senior Support', 3), (3, 'Development', None)])
emit('petition_categories', ['categoryID', 'displayName', 'description', 'queueID'], [
    (1, 'General', 'Questions and problems that fit no other category.', 1),
    (2, 'Stuck', 'You are stuck somewhere and cannot move.', 1),
    (3, 'Lost Items', 'Items or money disappeared.', 2),
    (4, 'Harassment', 'Another player is harassing you.', 2),
    (5, 'Bug Report', 'Something in the game does not work as it should.', 3),
])
emit('petition_properties', ['categoryID', 'propertyName'],
     [(2, 'Solar System'), (3, 'Item Name'), (3, 'Location'), (4, 'Character Name')])
# billMgr.GetBillTypes and cfg.billtypes (TEMPLATE DEFAULT names).
emit('billtypes', ['billTypeID', 'billTypeName'],
     [(1, 'Office Rental'), (2, 'Factory Slot Rental'), (3, 'Repair Bill'), (4, 'War Bill')])
TYPE_COLS = ['typeID', 'groupID', 'typeName', 'graphicID', 'volume', 'capacity', 'portionSize', 'marketGroupID',
             'chanceOfDuplicating']
TYPE_DEFAULTS = {'volume': 0.0, 'capacity': 0.0, 'portionSize': 1, 'chanceOfDuplicating': 0.0}
emit('types', TYPE_COLS, [[t.get(c, TYPE_DEFAULTS.get(c)) for c in TYPE_COLS] for t in types])
emit('dgmeffects', ['effectID', 'effectName', 'displayName', 'description', 'effectCategory', 'isOffensive',
                    'trackingSpeedAttributeID', 'rangeAttributeID', 'falloffAttributeID', 'dischargeAttributeID',
                    'durationAttributeID', 'graphicID'], effect_rows)
emit('dgmtypeeffects', ['typeID', 'effectID', 'isDefault'], type_effects)
emit('type_materials', ['typeID', 'materialTypeID', 'quantity'], type_materials)
emit('bptypes', ['blueprintTypeID', 'blueprintTypeName', 'description', 'parentBlueprintTypeID', 'productTypeID',
                 'techLevel', 'chanceOfReverseEngineering', 'basePrice'], bptypes)
emit('blueprint_info', ['blueprintTypeID', 'manufacturingTime', 'wastageFactor', 'researchMineralTime',
                        'researchCopyTime', 'researchProductivityTime'], blueprint_info)
emit('market_groups', ['marketGroupID', 'parentGroupID', 'marketGroupName', 'description', 'graphicID', 'hasTypes'],
     market_groups)
emit('type_prices', ['typeID', 'basePrice'], sorted(type_price.items()))
# cfg.shiptypes: every hull needs a row (infosvc, godma read it); 2003 hulls
# carried built-in module types, which these template hulls do not.
emit('shiptypes', ['shipTypeID'], [(t['typeID'],) for t in types
                                   if t['groupID'] in [group_ids[g] for g in seed.GROUP_CATEGORY['Ship'] if g in group_ids]])
emit('dgmtypeattribs', ['typeID', 'attributeID', 'value'], type_attrs)
emit('npc_corporations', ['corporationID', 'corporationName', 'tickerName', 'factionID', 'description', 'stationID'],
     corps)

# ================================================================== agents
# TEMPLATE DEFAULT: one agent per station, working for the station's owner
# (newBreedAgents; ui/station/agents/agents.py). Agents are NPC characters:
# owner rows with the Character type so cfg.eveowners treats them as such.
# Names are built from syllables, deterministically from the station ID.
AGENT_SYLLABLES = ['ka', 'ri', 'to', 'mi', 'sa', 'lo', 'ne', 'va', 'ur', 'is', 'en', 'ha', 'jo', 'ta', 'qu', 'el',
                   'om', 'dar', 'vek', 'sol', 'mar', 'tor', 'lin', 'zan']
BLOODLINE_OF_RACE = {}
for bl_i, (bl, info) in enumerate(BLOODLINES.items()):
    BLOODLINE_OF_RACE.setdefault(info[5], []).append(bl_i + 1)
agents = []
for i, st in enumerate(sorted(stations)):
    stid, _, owner, sid = st[0], st[1], st[2], st[3]
    h = int(hashlib.sha256(repr(('agent', stid)).encode()).hexdigest(), 16)
    first = ''.join(AGENT_SYLLABLES[(h >> (5 * k)) % len(AGENT_SYLLABLES)] for k in range(2)).capitalize()
    last = ''.join(AGENT_SYLLABLES[(h >> (5 * k + 20)) % len(AGENT_SYLLABLES)] for k in range(3)).capitalize()
    race = race_of_system(sid)
    bls = BLOODLINE_OF_RACE.get(race, [1])
    agents.append((3_000_001 + i, f'{first} {last}', owner, stid, RACE[race], bls[h % len(bls)], 1 + h % 3))
emit('agents', ['agentID', 'agentName', 'corporationID', 'stationID', 'raceID', 'bloodlineID', 'level'], agents)
emit('owners', ['ownerID', 'ownerName', 'typeID'],
     [(f, n, type_ids['Faction'] if 'Faction' in type_ids else CORP_TYPE) for f, (n, _) in sorted(FACTIONS.items())] +
     [(c[0], c[1], CORP_TYPE) for c in corps] +
     [(a[0], a[1], type_ids['Character']) for a in agents])
# const.locationUniverse is a location too: the map browser titles the
# universe view with cfg.evelocations.Get(const.locationUniverse).name.
emit('locations', ['locationID', 'locationName'],
     [(seed.values['locationUniverse'], 'Universe')] +
     [(k, o['itemName']) for k, o in sorted(objects.items())] + [(s[0], s[6]) for s in stations])
CEL_COLS = ['itemID', 'typeID', 'groupID', 'solarSystemID', 'constellationID', 'regionID', 'orbitID', 'x', 'y', 'z',
            'radius', 'itemName', 'celestialIndex', 'orbitIndex']
emit('celestials', CEL_COLS, [[c[k] for k in CEL_COLS] for c in celestials], chunk=2000)
emit('stargates', ['stargateID', 'destinationID'], sorted(stargates.items()), chunk=2000)
emit('stations', ['stationID', 'stationTypeID', 'corporationID', 'solarSystemID', 'constellationID', 'regionID',
                  'stationName', 'serviceMask', 'x', 'y', 'z'], stations)

print('-- GENERATED by tools/gen_content.py. Do not edit by hand.')
print(f'-- {len(regions)} regions, {len(constellations)} constellations, {len(systems)} systems, '
      f'{len(jumps) // 2} gate links (shipped map); {len(celestials)} celestials, {len(stations)} stations, '
      f'{len(schools)} schools (template).\n')
print('\n'.join(out))
