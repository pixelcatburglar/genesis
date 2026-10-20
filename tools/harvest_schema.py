"""Harvest the column names the build-1000 client reads off each static table.

The server defines every bulk-data header, and the client reads columns by
attribute name, so the decompiled client's attribute accesses ARE the schema.
For each cfg container this finds row expressions and records which attributes
are read from them:

  * chains:     cfg.invtypes.Get(x).typeName, cfg.invtypes[x].typeName
  * variables:  t = cfg.invtypes.Get(x) ... t.typeName   (same function)
  * loops:      for t in cfg.invtypes: ... t.typeName
  * row classes: self.<attr> inside cfg.py's InvType/InvGroup/... classes
  * filter rowsets: for a in cfg.dgmtypeattribs.get(id, []): a.value

usage: py -3 -I tools/harvest_schema.py decompiled/src > docs/client-schema.json
"""
import collections
import json
import os
import re
import sys

# cfg attribute -> bulk table key in initvals
CONTAINERS = {
    'invtypes': 'types', 'invgroups': 'groups', 'invcategories': 'categories',
    'invbptypes': 'bptypes', 'dgmattribs': 'dgmattribs', 'dgmeffects': 'dgmeffects',
    'dgmtypeattribs': 'dgmtypeattribs', 'dgmtypeeffects': 'dgmtypeeffects',
    'shiptypes': 'shiptypes', 'evegraphics': 'graphics', 'eveowners': 'owners',
    'evelocations': 'locations', 'evemessages': 'messages',
    'typesByGroups': 'types', 'groupsByCategories': 'groups',
}
# Row-class aliases (cfg.py __getattr__): not columns of these tables.
ALIASES = {'groups': {'name'}, 'categories': {'name', 'description'}, 'types': {'name'},
           'graphics': {'name', 'description'}, 'owners': {'name', 'description'},
           'locations': {'name', 'description'}}
# str methods read off name columns, e.g. row.typeName.lower() mis-parsed as a column.
STR_METHODS = {'lower', 'upper', 'endswith', 'startswith', 'strip', 'replace', 'split', 'find'}
# Row methods that return a row of another table.
ROW_METHODS = {'Type': 'types', 'Group': 'groups', 'Category': 'categories', 'Graphic': 'graphics',
               'Parent': 'groups'}
FILTER_ROWSETS = {'dgmtypeattribs', 'dgmtypeeffects', 'typesByGroups', 'groupsByCategories'}
ROW_CLASSES = {'InvGroup': 'groups', 'InvCategory': 'categories', 'InvType': 'types',
               'EveGraphics': 'graphics', 'EveOwners': 'owners', 'EveLocations': 'locations'}

# Attributes that belong to the container/row machinery, not to a column.
NOT_COLUMNS = {
    'Get', 'GetIfExists', 'Prime', 'header', 'line', 'id', 'data', 'get', 'has_key',
    'keys', 'values', 'items', 'iteritems', 'itervalues', 'iterkeys', 'Group', 'Category',
    'Types', 'Graphic', 'ShipType', 'Type', 'Parent', 'IsSystem', 'IsNPC', 'IsCharacter',
    'Station', 'IsHardware', 'keycolumn', 'keyidx', 'append', 'index',
    'GetMulti', 'HardwareType', 'Select', 'Sort', 'Filter', 'IndexRowset', 'FilterRowset',
    'lines', 'copy', 'idName', 'idName2', 'Index', 'GetLine',
}

IDENT = r'[A-Za-z_][A-Za-z0-9_]*'
CHAIN = re.compile(r'cfg\.(' + IDENT + r')(?:\.(?:Get|GetIfExists|get)\(|\[)')
ASSIGN = re.compile(r'^\s*(' + IDENT + r')\s*=\s*cfg\.(' + IDENT + r')(?:\.(?:Get|GetIfExists)\(|\[)')
LOOP = re.compile(r'^\s*for\s+(' + IDENT + r')\s+in\s+cfg\.(' + IDENT + r')\b(\.get\(|\[|\.itervalues\(|\.values\(|\s*:)?')
DEF = re.compile(r'^(\s*)def\s')
CLASS = re.compile(r'^class\s+(' + IDENT + r')')


def matching_close(s, i):
    """Index just past the bracket expression opening at s[i]."""
    depth = 0
    pairs = {'(': ')', '[': ']'}
    close = pairs[s[i]]
    for j in range(i, len(s)):
        if s[j] in '([':
            depth += 1
        elif s[j] in ')]':
            depth -= 1
            if depth == 0:
                return j + 1
    return len(s)


def main():
    root = sys.argv[1]
    found = collections.defaultdict(lambda: collections.defaultdict(set))

    def record(table, attr, where):
        if (attr not in NOT_COLUMNS and attr not in STR_METHODS and not attr.startswith('__')
                and attr not in ALIASES.get(table, ())):
            found[table][attr].add(where)

    for dirpath, _, files in os.walk(root):
        for fn in files:
            if not fn.endswith('.py'):
                continue
            path = os.path.join(dirpath, fn)
            rel = os.path.relpath(path, root).replace('\\', '/')
            lines = open(path, encoding='utf-8', errors='replace').read().splitlines()
            func_vars, func_colls = {}, {}
            klass = None
            for n, line in enumerate(lines, 1):
                where = f'{rel}:{n}'
                if DEF.match(line) and len(DEF.match(line).group(1)) <= 4:
                    func_vars, func_colls = {}, {}
                m = CLASS.match(line)
                if m:
                    klass = m.group(1)
                # 1. chains
                for m in CHAIN.finditer(line):
                    table = CONTAINERS.get(m.group(1))
                    if not table:
                        continue
                    end = matching_close(line, m.end() - 1)
                    rest = line[end:]
                    a = re.match(r'\.(' + IDENT + r')', rest)
                    if a:
                        record(table, a.group(1), where)
                # 1b. row methods: x.Type().typeName
                for m in re.finditer(r'\.(' + '|'.join(ROW_METHODS) + r')\(\)\.(' + IDENT + r')', line):
                    record(ROW_METHODS[m.group(1)], m.group(2), where)
                # 2. variables bound to rows. Subscripting a filter rowset yields
                # a list of rows, so its variable is a collection, not a row.
                m = ASSIGN.match(line)
                if m and m.group(2) in CONTAINERS:
                    if m.group(2) in FILTER_ROWSETS:
                        func_colls[m.group(1)] = CONTAINERS[m.group(2)]
                    else:
                        func_vars[m.group(1)] = CONTAINERS[m.group(2)]
                m = LOOP.match(line)
                if m and m.group(2) in CONTAINERS:
                    func_vars[m.group(1)] = CONTAINERS[m.group(2)]
                m = re.match(r'^\s*for\s+(' + IDENT + r')\s+in\s+(' + IDENT + r')\s*:', line)
                if m and m.group(2) in func_colls:
                    func_vars[m.group(1)] = func_colls[m.group(2)]
                for var, table in list(func_vars.items()):
                    for a in re.finditer(r'(?<![\w.])' + re.escape(var) + r'\.(' + IDENT + r')', line):
                        record(table, a.group(1), where)
                    if re.match(r'^\s*' + re.escape(var) + r'\s*=(?!=)', line) and not ASSIGN.match(line):
                        del func_vars[var]
                # 3. row classes in cfg.py
                if rel == 'sys/cfg.py' and klass in ROW_CLASSES:
                    for a in re.finditer(r'self\.(' + IDENT + r')', line):
                        record(ROW_CLASSES[klass], a.group(1), where)

    out = {t: {a: sorted(w)[:5] for a, w in sorted(cols.items())} for t, cols in sorted(found.items())}
    json.dump(out, sys.stdout, indent=1)


if __name__ == '__main__':
    main()
