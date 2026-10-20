"""Harvest every remote service call the build-1000 client can make.

Patterns: eve.RemoteSvc('svc').Method(...), sm.RemoteSvc(...),
<session>.ConnectToRemoteService('svc').Method(...), and the same reached
through a local variable (function scope), an attribute (`self.x = ...`, file
scope) or a method that returns the service (`def GetX(self): return
eve.RemoteSvc('svc')`, file scope). Bound-object calls (monikers) are in
tools/harvest_bound.py. Calls made through getattr(svc, name) are invisible
here (ui/shared/accountsvc.py: account.GetKeyMap/GetRefTypes/GetStatement/
GetJournal).

usage: py -3 -I tools/harvest_rpc.py decompiled/src > docs/client-rpc.json
"""
import collections, json, os, re, sys

SVC = r"""(?:RemoteSvc|ConnectToRemoteService)\(\s*'([A-Za-z_]\w*)'\s*(?:,[^)]*)?\)"""
PAT = re.compile(SVC + r"""\.([A-Za-z_]\w*)""")
VAR = re.compile(r"""^\s*([A-Za-z_][\w.]*)\s*=\s*[^=]*?""" + SVC + r"""\s*$""")
RET = re.compile(r"""^\s*return\s+[^=]*?""" + SVC + r"""\s*$""")
root = sys.argv[1]
calls = collections.defaultdict(lambda: collections.defaultdict(list))
for dirpath, _, files in os.walk(root):
    for fn in files:
        if not fn.endswith('.py'):
            continue
        path = os.path.join(dirpath, fn)
        rel = os.path.relpath(path, root).replace(os.sep, '/')
        lines = list(open(path, encoding='utf-8', errors='replace'))
        attrs, wrappers, fname = {}, {}, None
        for line in lines:
            d = re.match(r'\s*def\s+(\w+)\(', line)
            if d:
                fname = d.group(1)
            m = VAR.match(line)
            if m and '.' in m.group(1):
                attrs[m.group(1).split('.')[-1]] = m.group(2)
            m = RET.match(line)
            if m and fname:
                wrappers[fname] = m.group(1)
        local = {}
        for n, line in enumerate(lines, 1):
            if re.match(r'\s*def\s', line):
                local = {}
            for m in PAT.finditer(line):
                calls[m.group(1)][m.group(2)].append(f'{rel}:{n}')
            m = VAR.match(line)
            if m and '.' not in m.group(1):
                local[m.group(1)] = m.group(2)
            for var, svc in local.items():
                for m in re.finditer(r'(?<![\w.])' + re.escape(var) + r'\.([A-Za-z_]\w*)\(', line):
                    calls[svc][m.group(1)].append(f'{rel}:{n}')
            for var, svc in attrs.items():
                for m in re.finditer(r'\.' + re.escape(var) + r'\.([A-Za-z_]\w*)\(', line):
                    calls[svc][m.group(1)].append(f'{rel}:{n}')
            for w, svc in wrappers.items():
                for m in re.finditer(r'\b' + w + r'\(\)\.([A-Za-z_]\w*)\(', line):
                    calls[svc][m.group(1)].append(f'{rel}:{n}')
out = {s: {m: sorted(set(w)) for m, w in sorted(ms.items())} for s, ms in sorted(calls.items())}
json.dump(out, sys.stdout, indent=1)
