"""Generate db/static/02_seed.sql: the rows the client requires.

Inputs are all client-derived:
  docs/client-constants.json  (tools/harvest_consts.py) - every const.<X> read
  decompiled/src               - message IDs raised/formatted by the client
  EVE/res/messages.txt         - messages the client already ships

The client reads ~350 generated constants (const.<prefix><RowName>), so a row
with the right NAME must exist for each, or the screen that reads it dies with
AttributeError. Names round-trip exactly: cfg removes spaces and upper-cases
the first letter, so 'Asteroid Belt' -> const.groupAsteroidBelt.

IDs are NOT knowable from the client (the server assigned them), so this
template assigns them deterministically. Where the client constrains a value
(ID ranges, slot-flag ordering, inventory row indices) that is noted inline.
Everything marked TEMPLATE DEFAULT is a choice, safe to replace with real data.

usage: py -3 -I tools/gen_seed.py > db/static/02_seed.sql
"""
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CONSTS = json.load(open(os.path.join(ROOT, 'docs', 'client-constants.json')))


def rows(table):
    return sorted({v['row'] for v in CONSTS.get(table, {}).values()})


def words(camel):
    """'AsteroidBelt' -> 'Asteroid Belt'; joins back to the same constant."""
    return re.sub(r'(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])', ' ', camel)


def lower_first(s):
    return s[0].lower() + s[1:]


def q(v):
    if v is None:
        return 'NULL'
    if isinstance(v, (int, float)):
        return repr(v)
    return "'" + str(v).replace("'", "''") + "'"


out = []


def emit(table, cols, values):
    out.append(f'INSERT INTO static.{table} ({", ".join(chr(34) + c + chr(34) for c in cols)}) VALUES')
    out.append(',\n'.join('    (' + ', '.join(q(v) for v in row) + ')' for row in values) + ';\n')


# ----------------------------------------------------------- categories/groups
# Dynamic reads not visible to the literal harvester are added by hand:
EXTRA = {
    # ui/station/fitting/base_fitting.py:474 getattr(const, 'attribute%s' % each)
    'config.BulkData.dgmattribs': ['EmDamageResonance', 'ExplosiveDamageResonance',
                                   'KineticDamageResonance', 'ThermalDamageResonance'],
    # base_fitting.py:104 / shipui.py:346 getattr(const, 'flag%sSlot%s' % (FitKeys()[g], 0..7))
    'config.Flags': [f'{k}Slot{i}' for k in ('Hi', 'Med', 'Lo') for i in range(8)],
}
for t, names in EXTRA.items():
    for n in names:
        CONSTS.setdefault(t, {}).setdefault('*' + n, {'row': n})

categories = rows('config.BulkData.categories')
cat_id = {c: i + 1 for i, c in enumerate(categories)}
emit('categories', ['categoryID', 'categoryName'], [(cat_id[c], words(c)) for c in categories])

# TEMPLATE DEFAULT: which category each client-required group sits under.
GROUP_CATEGORY = {
    'Owner': ['Character', 'Corporation', 'Faction', 'System'],
    'Celestial': ['Asteroid', 'AsteroidBelt', 'Beacon', 'Biomass', 'CargoContainer', 'Cloud',
                  'Comet', 'Constellation', 'ConstructionPlatform', 'LargeCollidableObject', 'Moon',
                  'Planet', 'PlanetaryCloud', 'Region', 'SolarSystem', 'SpawnContainer', 'Stargate',
                  'Sun', 'Wreck'],
    'Ship': ['Battleship', 'Capsule', 'Cruiser', 'Frigate', 'Industrial', 'Rookieship', 'Titan'],
    'Module': ['CapacitorBooster', 'EnergyWeapon', 'HybridWeapon', 'MiningLaser',
               'MissileLauncher', 'ProjectileWeapon', 'ShieldBooster'],
    'Charge': ['Ammo', 'Missile', 'Torpedo'],
    'Drone': ['CombatDrone', 'MiningDrone'],
    'Entity': ['ConcordDrone', 'Convoy', 'ConvoyDrone', 'FactionDrone', 'LCODrone', 'PirateDrone',
               'PoliceDrone', 'ProtectiveSentryGun', 'ProximityDrone', 'RogueDrone', 'SentryGun',
               'TutorialDrone'],
    'Implant': ['Booster', 'Cyberimplant', 'Clone'],
    'Station': ['Station', 'MobileRefinery'],
    'Trading': ['Mineral'],
    'Accessories': ['Money', 'Voucher', 'Shares', 'Container', 'Commodity'],
    'Skill': ['Skill'],
    'Blueprint': ['Blueprint'],
}
groups = rows('config.BulkData.groups')
# Helper groups (not client constants) that host required types: Clone, Shares, ...
# typeBookmark sits in groupVoucher: ui/deco/item.py treats a groupVoucher item
# of typeBookmark as a bookmark copy.
helper_groups = sorted({g for gs in GROUP_CATEGORY.values() for g in gs} - set(groups))
all_groups = groups + helper_groups
group_cat = {g: c for c, gs in GROUP_CATEGORY.items() for g in gs}
missing = [g for g in all_groups if g not in group_cat]
if missing:
    sys.exit(f'groups without a category: {missing}')

# PROVEN IDs: the client's shipped universe map (res:/UI/Shared/Maps/cachefile.blue,
# see tools/gen_content.py) carries groupID/typeID 3/4/5 on every region,
# constellation and solar system row.
PROVEN_GROUP_IDS = {'Region': 3, 'Constellation': 4, 'SolarSystem': 5}
PROVEN_TYPE_IDS = {'Character': 1, 'Region': 3, 'Constellation': 4, 'SolarSystem': 5}


def assign_ids(names, proven):
    ids, used, nxt = dict(proven), set(proven.values()), 1
    for n in names:
        if n in ids:
            continue
        while nxt in used:
            nxt += 1
        ids[n] = nxt
        used.add(nxt)
    return {n: ids[n] for n in names}


group_id = assign_ids(all_groups, PROVEN_GROUP_IDS)
emit('groups', ['groupID', 'categoryID', 'groupName'],
     [(group_id[g], cat_id[group_cat[g]], words(g)) for g in all_groups])

# --------------------------------------------------------------------- types
# TEMPLATE DEFAULT: group of each client-required type. typeID 1 is the abstract
# Character type: owner rows for players use it (EveOwners.IsCharacter compares
# the type's group with const.groupCharacter).
TYPE_GROUP = {
    'Character': 'Character', 'Corporation': 'Corporation', 'Region': 'Region',
    'Constellation': 'Constellation', 'SolarSystem': 'SolarSystem', 'Moon': 'Moon',
    'Beacon': 'Beacon', 'PlanetGas': 'Planet', 'PlanetIce': 'Planet', 'PlanetSolid': 'Planet',
    'Ship': 'Rookieship', 'BasicClone': 'Clone', 'Bookmark': 'Voucher', 'Credits': 'Money',
    'CompanyShares': 'Shares', 'SmallSecureContainer': 'CargoContainer',
    'PlasticWrap': 'Container', 'FactoryFolder': 'Container', 'OfficeFolder': 'Container',
    'PlayerKill': 'Commodity', 'Tourists': 'Commodity',
    'Industry': 'Skill', 'Science': 'Skill', 'CorporationManagement': 'Skill',
    'Duplicating': 'Skill', 'ReverseEngineering': 'Skill',
}
types = rows('config.BulkData.types')
missing = [t for t in types if t not in TYPE_GROUP]
if missing:
    sys.exit(f'types without a group: {missing}')
types = ['Character'] + [t for t in types if t != 'Character']
type_id = assign_ids(types, PROVEN_TYPE_IDS)
emit('types', ['typeID', 'groupID', 'typeName'],
     [(type_id[t], group_id[TYPE_GROUP[t]], words(t)) for t in types])

# ------------------------------------------------------------- units / dogma
units = rows('config.Units')
# TEMPLATE DEFAULT display suffixes (infosvc shows value + displayName).
UNIT_DISPLAY = {'Milliseconds': 'ms', 'Percentage': '%', 'ModifierPercent': '%', 'InversedModifierPercent': '%',
                'InverseAbsolutePercent': '%', 'AttributePoints': 'points'}
emit('units', ['unitID', 'unitName', 'displayName'],
     [(i + 1, words(u), UNIT_DISPLAY.get(u, '')) for i, u in enumerate(units)])

# attributeName is lowerCamel: const.attribute<Name> upper-cases the first letter.
attribs = rows('config.BulkData.dgmattribs')
attr_id = {a: i + 1 for i, a in enumerate(attribs)}
emit('dgmattribs', ['attributeID', 'attributeName', 'displayName'],
     [(attr_id[a], lower_first(a), words(a)) for a in attribs])

# NB: 'loPower', not 'lowPower' - the constant is const.effectLoPower (base.py
# string-compares 'lowPower' in one dead branch; the constant wins).
effects = rows('config.BulkData.dgmeffects')
emit('dgmeffects', ['effectID', 'effectName', 'displayName'],
     [(i + 1, lower_first(e), words(e)) for i, e in enumerate(effects)])

# --------------------------------------------------------------------- flags
# Constraints (environment/godma.py:235,902, invCache.py:260):
#   flagSlotFirst <= every fitted slot <= flagSlotLast
#   flagLoSlot0 <= flagFixedSlot, and FixedSlot is itself a fitted slot
#   Lo/Med/Hi Slot0..7 are separate flags (base_fitting.py:104).
FLAGS = {
    'None': 0, 'Wallet': 1, 'Hangar': 4, 'Cargo': 5, 'Skill': 7, 'Reward': 8,
    **{f'LoSlot{i}': 11 + i for i in range(8)},
    **{f'MedSlot{i}': 19 + i for i in range(8)},
    **{f'HiSlot{i}': 27 + i for i in range(8)},
    'FixedSlot': 35, 'SlotFirst': 11, 'SlotLast': 35,
    'Pilot': 57, 'SkillInTraining': 61, 'DroneBay': 87, 'Booster': 88, 'Implant': 89,
    'FactorySlotFirst': 100, 'FactorySlotLast': 109,
    'ResearchFacilitySlotFirst': 110, 'ResearchFacilitySlotLast': 119,
    'AutoFit': 129,
}
missing = [f for f in rows('config.Flags') if f not in FLAGS]
if missing:
    sys.exit(f'flags without a value: {missing}')
# Several names share a value (SlotFirst = LoSlot0). Each name is its own row:
# the client only builds name -> value constants from them.
out.append('-- flagID is not unique: SlotFirst/LoSlot0 and SlotLast/FixedSlot alias.')
out.append('ALTER TABLE static.flags DROP CONSTRAINT flags_pkey;')
out.append('ALTER TABLE static.flags ADD PRIMARY KEY ("flagName");\n')
emit('flags', ['flagID', 'flagName'], [(v, words(k)) for k, v in sorted(FLAGS.items(), key=lambda kv: kv[1])])

# ------------------------------------------------------------ races/bloodlines
# Bloodline -> race from the shipped face assets: res/Model/Face/<Race>/<Bloodline>/.
# TEMPLATE DEFAULT: race IDs are a bitmask.
RACES = {'Caldari': 1, 'Minmatar': 2, 'Amarr': 4, 'Gallente': 8, 'Jove': 16}
BLOODLINES = [('Deteis', 'Caldari'), ('Civire', 'Caldari'), ('Sebiestor', 'Minmatar'),
              ('Brutor', 'Minmatar'), ('Amarr', 'Amarr'), ('NiKunni', 'Amarr'),
              ('Gallente', 'Gallente'), ('Intaki', 'Gallente')]
assert sorted(RACES) == rows('config.Races')
assert sorted(b for b, _ in BLOODLINES) == rows('config.Bloodlines')
emit('races', ['raceID', 'raceName'], [(v, k) for k, v in RACES.items()])
emit('bloodlines', ['bloodlineID', 'bloodlineName', 'raceID'],
     [(i + 1, words(b), RACES[r]) for i, (b, r) in enumerate(BLOODLINES)])

# ----------------------------------------------------------------- constants
# Values the client itself proves are listed first; the rest are TEMPLATE
# DEFAULT enums (only compared for equality) assigned per family.
PROVEN = {
    # inventory row header, hardcoded twice (cfg.py ItemsRecordset2, godma.py:339)
    'ixItemID': 0, 'ixTypeID': 1, 'ixOwnerID': 2, 'ixLocationID': 3, 'ixFlag': 4,
    'ixSingleton': 6, 'ixQuantity': 7, 'ixGroupID': 8, 'ixCategoryID': 9, 'ixCustomInfo': 10,
}
DEFAULTS = {
    # ms between michelle ticks (remote/michelle.py:190)
    'simulationTimeStep': 1000,
    # divides blue-time deltas (100 ns units) into ms (godma.py:635, michelle.py:332)
    'dgmTauConstant': 10000,
    # metres
    'minWarpDistance': 150000, 'maxDockingDistance': 2500,
    'maxStargateJumpingDistance': 2500, 'maxCargoContainerTransferDistance': 2500,
    # owner IDs must stay <= 15 to count as system owners (cfg.py EveOwners.IsSystem)
    'ownerNone': 0, 'ownerSystem': 1, 'ownerStation': 2, 'ownerMarket': 3, 'ownerCONCORD': 4,
    'costCloneContract': 5000, 'corpLogoChangeCost': 100,
    'gangLeaderRole': 1, 'rentalPeriodOffice': 30,
}
# chrattr* name character attributes: use their dgmattribs IDs.
for a in ('Charisma', 'Intelligence', 'Memory', 'Perception', 'Willpower'):
    DEFAULTS['chrattr' + a] = attr_id[a]
# Bitmask families: station services, corp roles.
BITMASK_PREFIXES = ('stationService', 'corpRole')
plain = rows('config.BulkData.constants')
families = {}
for name in plain:
    if name in PROVEN or name in DEFAULTS:
        continue
    fam = re.match(r'[a-z]+', name).group(0)
    families.setdefault(fam, []).append(name)
values = {**PROVEN, **DEFAULTS}
for fam, names in sorted(families.items()):
    # <fam>None / <fam>Undefined are 0; the others count from 1.
    counted = [n for n in sorted(names) if not (n.endswith('None') or n.endswith('Undefined'))]
    for i, name in enumerate(sorted(names)):
        if name.startswith(BITMASK_PREFIXES):
            values[name] = 1 << i
        elif fam == 'location':
            values[name] = 1 + i  # abstract location IDs, far below 30,000,000
        else:
            values[name] = counted.index(name) + 1 if name in counted else 0
missing = [c for c in plain if c not in values]
assert not missing, missing
emit('constants', ['constantID', 'constantValue'], [(k, values[k]) for k in sorted(plain)])

# ------------------------------------------------------------------ messages
# Literal message IDs the client raises or formats that res:/messages.txt does
# not ship. A missing one is an ErrMessageNotFound (inside the exception
# handler for UserErrors). TEMPLATE DEFAULT text = the ID in words; type '' is
# a silent text resource, 'question' where the client passes buttons.
shipped = set()
for line in open(os.path.join(ROOT, 'EVE', 'res', 'messages.txt'), encoding='latin-1'):
    line = line.strip()
    if line and not line.startswith('#') and '=' in line:
        shipped.add(line.split('=', 1)[0].strip().split('.', 1)[0])
# The literal must be the whole first argument ('NeoCom' + action is not a key).
msg_re = re.compile(r"""(?:\.Message|\bMessage|UserError|cfg\.Format|GetMessage)\(\s*'([A-Za-z_][A-Za-z0-9_]*)'(\s*[,)].*)""")
# eve.Message(('A', 'B')[i]) (charsel.py) and getattr(self, 'x', 'Default') (container.py).
msg_tuple_re = re.compile(r"""\.Message\(\(((?:\s*'[A-Za-z_]\w*'\s*,?)+)\)\[""")
msg_default_re = re.compile(r"""\.Message\(getattr\([^,]+,\s*'\w+',\s*'([A-Za-z_]\w*)'\)""")
found = {}
# Placeholders: only keys that EVERY call site passes in a literal dict, since
# a format key missing from the dict is an ErrMessageKeyError in the client.
arg_keys = {}
for dirpath, _, files in os.walk(os.path.join(ROOT, 'decompiled', 'src')):
    for fn in files:
        if fn.endswith('.py'):
            for line in open(os.path.join(dirpath, fn), encoding='utf-8', errors='replace'):
                for m in msg_tuple_re.finditer(line):
                    for mid in re.findall(r"'(\w+)'", m.group(1)):
                        if mid not in shipped:
                            found.setdefault(mid, '')
                for m in msg_default_re.finditer(line):
                    if m.group(1) not in shipped:
                        found.setdefault(m.group(1), '')
                for m in msg_re.finditer(line):
                    mid, rest = m.group(1), m.group(2)
                    if mid in shipped:
                        continue
                    asks = re.search(r'(?:uix|triui)\.(YESNO|OKCANCEL|YESNOCANCEL)', rest[:400])
                    if asks or found.get(mid) == 'question':
                        found[mid] = 'question'
                    else:
                        found.setdefault(mid, '')
                    lit = re.match(r'\s*,\s*\{([^{}]*)\}', rest)
                    keys = set(re.findall(r"'([A-Za-z_]\w*)'\s*:", lit.group(1))) if lit else set()
                    arg_keys[mid] = keys if mid not in arg_keys else arg_keys[mid] & keys


# Keys the client BUILDS at run time (all sounds, played by eve.Message; a
# missing one raises ErrMessageNotFound and aborts the caller):
#   neocom.py Position: 'NeoCom' + action, action 'In' / 'Out'
#   station/base.py _LoadSvc: 'LoadSvc_%s%s' % (service, 'Out'|'In'),
#     SetCaption: '%sHeader' % service, for the lobby's service buttons
#   fitting/base_fitting.py: 'fittingSlot%s' % uix.FitKeys() ('Hi', 'Med', 'Lo')
# TEMPLATE DEFAULT: silent (no urlAudio) where res:/messages.txt has no sound.
lobby_src = open(os.path.join(ROOT, 'decompiled', 'src', 'ui', 'station', 'lobby.py'), encoding='utf-8').read()
lobby_services = re.findall(r"\('([a-z ]+)', '\d+_\d+', \(", lobby_src[lobby_src.index('services = ['):])
assert 'medical' in lobby_services and 'storage' in lobby_services, lobby_services
DYNAMIC_MESSAGES = ['NeoComIn', 'NeoComOut'] + [f'fittingSlot{k}' for k in ('Hi', 'Med', 'Lo')]
for svc in lobby_services:
    DYNAMIC_MESSAGES += [f'LoadSvc_{svc}In', f'LoadSvc_{svc}Out', f'{svc}Header']
for mid in DYNAMIC_MESSAGES:
    if mid not in shipped:
        found.setdefault(mid, '')


def message_text(mid):
    keys = sorted(arg_keys.get(mid, ()))
    return words(mid) + (': ' + ', '.join(f'%({k})s' for k in keys) if keys else '')


# Keys the SERVER raises as UserError. The client's handler shows them with
# eve.Message, so each must exist (crates/server raises exactly these).
SERVER_MESSAGES = {
    'AuthenticationFailure': ('infomodal', 'Login Failed::The user name or password is incorrect.'),
    'CharNameTaken': ('info', "Name Taken::The name '%(name)s' is already in use. Please choose another."),
    'CharNameInvalid': ('info', 'Invalid Name::%(reason)s'),
    'CharSlotsFull': ('info', 'No Free Slot::You already have three characters.'),
    'CharCreationInvalid': ('info', 'Character Creation Failed::%(reason)s'),
    'NotYetImplemented': ('info', 'Not Implemented::%(what)s is not available on this server yet.'),
    # godma.OnDamageMessage(key, args) lines; source/target become bracket names.
    'AttackHit': ('notify', '%(source)s hits %(target)s for %(damage)s damage.'),
    'AttackMiss': ('notify', '%(source)s misses %(target)s.'),
    'MiningCargoFull': ('notify', 'Your cargo hold is full.'),
    # Facility dialogs: facility.py raises the key its subclass names
    # (base_factories / base_researchfacility GetMessage*), so no literal call
    # shows them to the harvester.
    'AskRentFactorySlot': ('question', 'Rent Factory Slot::Renting a factory slot costs %(cost)s ISK. Rent one?'),
    'AskRentResearchFacilitySlot': ('question', 'Rent Research Slot::Renting a research slot costs %(cost)s ISK. Rent one?'),
    'AskRentFactorySlotForCorporateUse': ('question', 'Corporate Use::Rent the factory slot for your corporation?'),
    'AskRentResearchFacilitySlotForCorporateUse': ('question', 'Corporate Use::Rent the research slot for your corporation?'),
    'NoFactorySlotsAreAvailableForRenting': ('info', 'No Free Slots::No factory slots are available for rent here.'),
    'NoResearchFacilitySlotsAreAvailableForRenting': ('info', 'No Free Slots::No research slots are available for rent here.'),
    'AskCancelRentOfFactorySlot': ('question', 'Cancel Rental::Give up this factory slot?'),
    'AskCancelRentOfResearchFacilitySlot': ('question', 'Cancel Rental::Give up this research slot?'),
    'IngredientsMissingForBlueprint': ('info', 'Missing Ingredients::Your hangar lacks what this job needs:<br>%(minerals)s'),
}
# TEMPLATE DEFAULT agent dialogue (cfg.GetCannedMessage 'agents::<key>[A]' is
# what the agent says, '[C]' what the pilot answers; %(...)s are the props
# newBreedAgents.Interact returns).
AGENT_DIALOGUE = {
    'agents::root[A]': 'Greetings, %(char.name)s. I am %(agent.name)s of %(agent.corp)s. What can I do for you?',
    'agents::root::remote[A]': 'Greetings, %(char.name)s. Come to %(agent.station)s if you want to discuss new work.',
    'agents::mission::ask[C]': 'Do you have any work for me?',
    'agents::mission::ask[A]': '%(mission.text)s The pay is %(mission.reward)s ISK. Will you do it?',
    'agents::mission::accept[C]': 'I will take the job.',
    'agents::mission::accept[A]': 'Good. %(mission.howto)s',
    'agents::mission::decline[C]': 'Not right now.',
    'agents::mission::decline[A]': 'As you wish. Come back if you change your mind.',
    'agents::mission::status[C]': 'About the job you gave me...',
    'agents::mission::status[A]': '%(mission.text)s %(mission.howto)s',
    'agents::mission::complete[C]': 'The job is done.',
    'agents::mission::complete[A]': 'Excellent work, %(char.name)s. %(mission.reward)s ISK has been transferred to your account.',
    'agents::mission::notdone[A]': 'That is not finished yet. %(mission.howto)s',
    'agents::mission::quit[C]': 'I want to give up this job.',
    'agents::mission::quit[A]': 'Disappointing. Perhaps someone else will see it through.',
    'agents::mission::busy[A]': 'You already have a job from me. Finish it first.',
    'agents::.quit.root[C]': 'Goodbye.',
    'agents::mission::journal::briefing': '%(mission.text)s',
    'agents::mission::journal::help': '%(mission.howto)s',
}
SERVER_MESSAGES.update({k: ('', v) for k, v in AGENT_DIALOGUE.items()})
assert not set(SERVER_MESSAGES) & shipped, set(SERVER_MESSAGES) & shipped
rows_ = {k: (t, message_text(k)) for k, t in found.items()}
# Every other key the server raises (user_error("Key", ...) in crates/server)
# gets an info box with its name in words, unless the client ships it or
# already references it above.
for dirpath, _, files in os.walk(os.path.join(ROOT, 'crates', 'server', 'src')):
    for fn in files:
        if fn.endswith('.rs'):
            src = open(os.path.join(dirpath, fn), encoding='utf-8').read()
            for mid in re.findall(r'user_error\(\s*"([A-Za-z_][A-Za-z0-9_]*)"', src):
                if mid not in shipped and mid not in rows_:
                    rows_[mid] = ('info', words(mid))
rows_.update(SERVER_MESSAGES)
emit('messages', ['messageID', 'messageType', 'messageText'],
     [(k, t, txt) for k, (t, txt) in sorted(rows_.items())])

SQL = '\n'.join(out)

if __name__ == '__main__':
    print('-- GENERATED by tools/gen_seed.py from client-derived inputs. Do not edit by hand.')
    print('-- Rows every build-1000 client requires; IDs are template defaults.\n')
    print(SQL)
