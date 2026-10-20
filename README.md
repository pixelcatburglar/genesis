# Genesis

A server for the **EVE Online build 1000** client (May 2003, version 10.0,
codename *Gemini*), written in Rust on PostgreSQL. The client is the only
specification: every wire format and table here is reversed from the shipped
client, cited to the decompiled source or DLL address it came from.

## Run

```sh
docker compose up --build
```

This starts PostgreSQL 17 and the server. The server builds the static schema
from `db/static/` (rebuilt whenever those files change), applies the
`db/game/` migrations, and listens on **26000**. In the client's login screen
use host `localhost`, port `26000`, and any name/password: accounts are created
on first login (`GENESIS_AUTO_CREATE_ACCOUNTS=0` turns that off). Passwords are
stored in plain text, so this is not for public hosting.

**blue.dll does not need patching.** The patcher neutralises
`CryptVerifySignatureA` checks, and build 1000 has none:
`tools/check_blue_crypto.py` finds no `Crypt*` imports, no
`CryptVerifySignature` string, no `cryptoPack`/`Placebo`, and no
NTE_BAD_SIGNATURE constant. Its only "crypt" strings belong to the statically
linked PyCrypto cipher modules. (This build predates 3913, which the
patcher's README already documents as needing no patch.)

`py -3 -I tools/fake_client.py HOST PORT USER PASS --create --select --station
--space` plays the client's side against a running server: login, the
creation wizard, character select, station entry, undock, a warp to a
stargate, a jump, a warp to the next system's station, and docking.
`--services` tours the neocom services, `--combat` mines and fights belt
pirates, and `--industry` (a GM account: role | 8, for `/create`) runs agents,
the star map, reprocessing, repair, a factory job, couriers and brokers.

## Status

Implemented and exercised end to end by the scripted client:

* **login**: version check, accounts, auth Ping, the cached login tables
* **character creation**: races (Jove hidden, as face.py has no Jove faces),
  bloodlines, ancestries, schools, departments, fields, specialities with
  their shipped logos; CreateCharacter with the wizard's validation
* **station**: hangar, inventory, dogma, corp logon data, chat, wallet,
  bookmarks/vouchers, mail, petitions, news, standings, bounties, medical,
  paint shop, skills and training, slash commands for GMs
* **corporations**: NPC and player corps, roles, applications, votes, wars,
  shares, offices, corp wallet
* **gangs**, **custom chat channels**
* **space**: the real 2003 universe; destiny ballparks; warp, gates, docking;
  targeting, turrets, mining, shield boosting, drones, belt pirates with
  bounties and loot, ship destruction and capsules
* **market**: NPC standing orders, player buy/sell orders, escrow, history
* **agents**: NPC agents at every station with courier and supply missions
  (dialogue, journal, rewards, standings)
* **industry**: reprocessing, repair, blueprints, factory and research slots
  (rental, manufacturing, copying, ME/PE research, duplication, reverse
  engineering), player courier contracts, brokers, direct player trades
* **star map** (`config.GetMapObjects` and friends)

Direct trades need two clients and are not in the scripted tour.
Unimplemented calls answer `None` and are logged as `UNIMPLEMENTED`.

## Layout

| path | |
|---|---|
| `crates/marshal` | blue.marshal codec (docs/protocol.md §2) |
| `crates/proto` | framing, machoNet packets, GPCS wrapping |
| `crates/server` | the `genesis` binary: login, services, ballparks (`space/`) |
| `db/static` | client-contract tables + world content ([docs/static-data.md](docs/static-data.md)) |
| `db/game` | accounts, characters, items (incremental migrations) |
| `docs/` | protocol spec and the harvested client contract (`client-*.json`) |
| `tools/` | reverse-engineering and content scripts (below) |
| `EVE/` | the client install (not modified) |
| `decompiled/` | generated: `src/` Python, `dis/` bytecode listings, `native/` Ghidra C |
| `client_res/` | generated: unpacked `.stuff` archives + `index.jsonl` |

## Reverse-engineering toolchain

| step | command |
|---|---|
| Python scripts | `py -3 -I tools/decompile_client.py EVE decompiled` unpickles `script/compiled.code` (299 modules, Python 2.2) and runs uncompyle6 through `tools/uncompyle22.py`, which fixes two 2.x defects: constant lists printed as `[1, 2, 3...]` and list comprehensions printed as `[_[1] for ...]` |
| native DLLs | `tools/ghidra/run.sh EVE/blue.dll` runs headless Ghidra 12 (portable JDK in `_tools/`) → `decompiled/native/<dll>.c`; `PyMethodFunctions`/`DataRefFunctions` first create the functions reached only through Python method tables and vtables, which Ghidra's analysis misses |
| archives | `py -3 -I tools/unstuff.py EVE client_res --index-only resTextures.stuff ...` |
| blue objects | `tools/blue_image.py` (reader for .blue files and marshal tag 0x18), `tools/destiny_members.py` (a class's member table) |
| client contract | `tools/harvest_schema.py`, `harvest_consts.py`, `harvest_rpc.py` → `docs/client-*.json` |
| seed rows | `py -3 -I tools/gen_seed.py > db/static/02_seed.sql` |
| world | `py -3 -I tools/gen_content.py > db/static/04_content.sql` (real universe from the client's map cache + template content) |
| x86 helpers | `tools/pe_xref.py` (string → referencing code), `tools/pe_find_imm.py` (immediate operand search) |
