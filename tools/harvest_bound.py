"""Harvest every bound-object (moniker) call the build-1000 client can make.

util/moniker.py's getters each bind one service; the objects they return (and
the inventories invbroker hands out) are called like local objects. This
follows those values through functions that return them (any file), locals
(function scope) and self/attribute names (file scope), and records
`<value>.Method(` per service. Heuristic, like harvest_rpc.py.

usage: py -3 -I tools/harvest_bound.py decompiled/src > docs/client-bound.json
"""
import collections, json, os, re, sys

GETTERS = {
    'CharGetDogmaLocation': 'dogmaIM', 'GetShipAccess': 'ship', 'GetEntityAccess': 'entity',
    'CharGetSkillHandler': 'skillMgr', 'GetBrokerManager': 'broker', 'GetBrokerManagerEx': 'broker',
    'GetFactoryManager': 'factory', 'GetFactoryManagerEx': 'factory', 'GetCorpStationManager': 'corpStationMgr',
    'GetCorpStationManagerEx': 'corpStationMgr', 'GetSolarSystemInventoryMgr': 'invbroker',
    'GetStationInventoryMgr': 'invbroker', 'GetInventoryMgr': 'invbroker', 'GetAggressionManager': 'aggressionMgr',
    'GetBallPark': 'beyonce', 'GetMarket': 'market', 'GetCourierMissionCreator': 'missionMgr',
    # invbroker's sub-objects
    'GetInventory': 'inventory', 'GetInventoryFromId': 'inventory',
    # gameui/michelle/godma/container wrappers that cache the value in an attribute
    'GetRemotePark': 'beyonce', 'GetDogmaLM': 'dogmaIM', 'GetSkillHandler': 'skillMgr',
    'GetMarketRegion': 'market', 'GetShell': 'inventory',
}
SKIP = {'SetSessionCheck', 'Unbind', 'Bind', 'GetSession'}


def files(root):
    for dirpath, _, names in os.walk(root):
        for fn in names:
            if fn.endswith('.py') and fn != 'moniker.py':
                path = os.path.join(dirpath, fn)
                yield os.path.relpath(path, root).replace(os.sep, '/'), \
                    open(path, encoding='utf-8', errors='replace').read().split('\n')


root = sys.argv[1]
src = list(files(root))
# Pass 1: functions returning a getter's value become getters (to a fixpoint).
while True:
    get = '|'.join(GETTERS)
    added = 0
    for _, lines in src:
        fname = None
        for line in lines:
            d = re.match(r'\s*def\s+(\w+)\(', line)
            if d:
                fname = d.group(1)
            # the value itself, not a call on it: `return x.GetInventory(a, b)`
            r = re.match(r'\s*return\s.*?\b(' + get + r')\([^()]*(?:\([^()]*\)[^()]*)*\)\s*$', line)
            if r and fname and fname not in GETTERS and not fname.startswith('__'):
                GETTERS[fname] = GETTERS[r.group(1)]
                added += 1
    if not added:
        break
GET = '|'.join(GETTERS)
CALL = re.compile(r'\b(' + GET + r')\([^()]*(?:\([^()]*\)[^()]*)*\)\.([A-Za-z_]\w*)\(')
ASSIGN = re.compile(r'^\s*([A-Za-z_][\w.]*)\s*=\s*[^=]*?\b(' + GET + r')\(')

calls = collections.defaultdict(lambda: collections.defaultdict(set))
for rel, lines in src:
    attrs = {'inventorymgr': 'invbroker'}  # sys/eve.py __getattr__('inventorymgr')
    for line in lines:
        m = ASSIGN.match(line)
        if m and '.' in m.group(1):
            attrs[m.group(1).split('.')[-1]] = GETTERS[m.group(2)]
    # def GetMoniker(self): ... return self.moniker  (svc.marketQuote)
    wrappers, fname = {}, None
    for line in lines:
        d = re.match(r'\s*def\s+(\w+)\(', line)
        if d:
            fname = d.group(1)
        r = re.match(r'\s*return\s+self\.(\w+)\s*$', line)
        if r and fname and r.group(1) in attrs:
            wrappers[fname] = attrs[r.group(1)]
    local = {}
    for n, line in enumerate(lines, 1):
        if re.match(r'\s*def\s', line):
            local = {}
        m = ASSIGN.match(line)
        if m and '.' not in m.group(1):
            local[m.group(1)] = GETTERS[m.group(2)]
        for m in CALL.finditer(line):
            calls[GETTERS[m.group(1)]][m.group(2)].add(f'{rel}:{n}')
        for var, svc in local.items():
            for m in re.finditer(r'(?<![\w.])' + re.escape(var) + r'\.([A-Za-z_]\w*)\(', line):
                calls[svc][m.group(1)].add(f'{rel}:{n}')
        for var, svc in attrs.items():
            for m in re.finditer(r'\.' + re.escape(var) + r'\.([A-Za-z_]\w*)\(', line):
                calls[svc][m.group(1)].add(f'{rel}:{n}')
        for w, svc in wrappers.items():
            for m in re.finditer(r'\.' + w + r'\(\)\.([A-Za-z_]\w*)\(', line):
                calls[svc][m.group(1)].add(f'{rel}:{n}')
out = {s: {m: sorted(w) for m, w in sorted(ms.items()) if m not in SKIP and m not in GETTERS}
       for s, ms in sorted(calls.items())}
json.dump(out, sys.stdout, indent=1)
