"""Harvest every const.<Name> the build-1000 client reads, classified by source.

cfg.GotInitData generates constants from row names (InsertConstantsFromRowset):
the row name with spaces removed and its first letter upper-cased, prefixed by
the table's prefix ('type' + 'Velator' -> const.typeVelator). Everything else
const exposes comes from config.BulkData.constants (constantID -> int value)
or res:/constants.txt. So every const.X the client dereferences is a row (or a
constant) the server's data must provide.

usage: py -3 -I tools/harvest_consts.py decompiled/src EVE/res/constants.txt > docs/client-constants.json
"""
import collections
import json
import os
import re
import sys

# prefix -> initvals table, from cfg.GotInitData's InsertConstantsFromRowset calls
PREFIXES = {
    'category': 'config.BulkData.categories', 'group': 'config.BulkData.groups',
    'type': 'config.BulkData.types', 'attribute': 'config.BulkData.dgmattribs',
    'effect': 'config.BulkData.dgmeffects', 'billType': 'config.BulkData.billtypes',
    'role': 'config.Roles', 'flag': 'config.Flags', 'race': 'config.Races',
    'bloodline': 'config.Bloodlines', 'statistic': 'config.Statistics',
    'unit': 'config.Units', 'channelType': 'config.ChannelTypes',
    'encyclopediaType': 'config.EncyclopediaTypes',
}
# Longest prefix first so 'billType' wins over a hypothetical 'bill'.
ORDER = sorted(PREFIXES, key=len, reverse=True)


def classify(name):
    for p in ORDER:
        if name.startswith(p) and len(name) > len(p) and name[len(p)].isupper():
            return PREFIXES[p], name[len(p):]
    return 'config.BulkData.constants', name


def main():
    root, constants_txt = sys.argv[1], sys.argv[2]
    shipped = set()
    for line in open(constants_txt, encoding='latin-1'):
        line = line.strip()
        if line and not line.startswith('#') and '=' in line:
            shipped.add(line.split('=', 1)[0].strip())
    refs = collections.defaultdict(set)
    for dirpath, _, files in os.walk(root):
        for fn in files:
            if fn.endswith('.py'):
                path = os.path.join(dirpath, fn)
                rel = os.path.relpath(path, root).replace('\\', '/')
                for n, line in enumerate(open(path, encoding='utf-8', errors='replace'), 1):
                    for m in re.finditer(r'\bconst\.([A-Za-z_][A-Za-z0-9_]*)', line):
                        refs[m.group(1)].add(f'{rel}:{n}')
    out = collections.defaultdict(dict)
    for name in sorted(refs):
        if name in shipped:
            table, row = 'res:/constants.txt', name
        else:
            table, row = classify(name)
        out[table][name] = {'row': row, 'refs': len(refs[name]), 'first': sorted(refs[name])[0]}
    json.dump(out, sys.stdout, indent=1, sort_keys=True)


if __name__ == '__main__':
    main()
