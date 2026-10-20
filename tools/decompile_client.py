"""Decompile the build-1000 client's script/compiled.code.

compiled.code is a binary pickle: a list of (b'script:/path.py', marshalled code
object) tuples.  The code objects use the Python 2.2 marshal layout.  Each one is
wrapped in a 2.2 .pyc header and handed to uncompyle6; a full disassembly is
written beside every module as ground truth, because uncompyle6's 2.2 support
is imperfect (failures land in decompiled/_failures.txt).

usage: py -3 -I tools/decompile_client.py <EVE dir> <out dir>
"""
import concurrent.futures
import os
import pickle
import struct
import subprocess
import sys

MAGIC_22 = struct.pack('<H', 60717) + b'\r\n'
TIMEOUT = 300
UNCOMPYLE = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'uncompyle22.py')


def write_pyc(path, code):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, 'wb') as f:
        f.write(MAGIC_22 + struct.pack('<I', 0x3EB5B9BD) + code)


def decompile(pyc, py, dis):
    ok = True
    try:
        r = subprocess.run([sys.executable, UNCOMPYLE, pyc],
                           capture_output=True, timeout=TIMEOUT)
        out = r.stdout.decode('utf-8', 'replace')
        if r.returncode != 0 or 'Parse error' in out or 'Deparsing stopped' in out:
            ok = False
            out += '\n# --- uncompyle6 stderr ---\n' + ''.join(
                '# ' + l + '\n' for l in r.stderr.decode('utf-8', 'replace').splitlines())
    except subprocess.TimeoutExpired:
        ok, out = False, '# uncompyle6 timed out\n'
    with open(py, 'w', encoding='utf-8') as f:
        f.write(out)
    r = subprocess.run([sys.executable, '-m', 'xdis.bin.pydisasm', '-F', 'extended', pyc],
                       capture_output=True, timeout=TIMEOUT)
    if r.returncode != 0:
        r = subprocess.run([sys.executable, '-m', 'xdis.bin.pydisasm', pyc],
                           capture_output=True, timeout=TIMEOUT)
    with open(dis, 'w', encoding='utf-8') as f:
        f.write(r.stdout.decode('utf-8', 'replace'))
    return ok


def main():
    eve, out = sys.argv[1], sys.argv[2]
    entries = pickle.load(open(os.path.join(eve, 'script', 'compiled.code'), 'rb'),
                          encoding='bytes')
    jobs = []
    for name, code in entries:
        rel = name.decode().split(':/', 1)[1]
        pyc = os.path.join(out, 'pyc', rel + 'c')
        write_pyc(pyc, code)
        py = os.path.join(out, 'src', rel)
        dis = os.path.join(out, 'dis', rel[:-3] + '.dis')
        os.makedirs(os.path.dirname(py), exist_ok=True)
        os.makedirs(os.path.dirname(dis), exist_ok=True)
        jobs.append((rel, pyc, py, dis))
    failures = []
    with concurrent.futures.ThreadPoolExecutor(os.cpu_count()) as ex:
        futs = {ex.submit(decompile, *j[1:]): j[0] for j in jobs}
        for fut in concurrent.futures.as_completed(futs):
            if not fut.result():
                failures.append(futs[fut])
    with open(os.path.join(out, '_failures.txt'), 'w') as f:
        f.write('\n'.join(sorted(failures)) + '\n')
    print(f'{len(jobs)} modules, {len(failures)} with decompile errors')


if __name__ == '__main__':
    main()
