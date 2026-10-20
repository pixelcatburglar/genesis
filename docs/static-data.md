# Static data: real universe + client-derived template

The build-1000 client ships **almost no static data**. Every table (types,
groups, dogma, owners, locations, messages, ...) arrives from the server at
login in `AuthenticationRsp` (sys/cfg.py `GotInitData`). This is why the
Elysian SDE extractor cannot help with this build: it reads FSD `ResFiles` or
pre-FSD `bulkdata/`, and build 1000 has neither. The `.stuff` archives
(`tools/unstuff.py` → `client_res/`) hold art, UI and sound, plus
`messages.txt`, `constants.txt`, `strings.txt` and `facetree.txt`.

**The universe is the exception.** `res:/UI/Shared/Maps/cachefile.blue` is a
UIWindow whose `data` member is a marshal stream the original developers wrote
from their server (ui/shared/maps/map.py CreateCacheFile). It holds all 66
regions, 780 constellations and 5,334 solar systems with names, positions,
bounding boxes, security, luminosity and faction, the full stargate graph
(6,796 links) and the 30 map landmarks. `tools/gen_content.py` decodes it, so
the map is real 2003 CCP data. It also proves groupID/typeID 3/4/5 for
region/constellation/solar system.

Everything else is the **client's contract**, recovered from what the client
reads:

| file | what it is | produced by |
|---|---|---|
| `db/static/01_schema.sql` | the 23 login tables, columns = wire headers | by hand from the harvests below |
| `db/static/02_seed.sql` | rows the client cannot run without | `tools/gen_seed.py` |
| `db/static/03_world_schema.sql` | map, celestials, stations, creation content | by hand from the client code that reads them |
| `db/static/04_content.sql` | real universe + template content | `tools/gen_content.py` |
| `db/game/*.sql` | accounts, characters, items | incremental migrations |
| `docs/client-schema.json` | every column read, with call sites | `tools/harvest_schema.py` |
| `docs/client-constants.json` | every `const.X` read, by source table | `tools/harvest_consts.py` |
| `docs/client-rpc.json` | every remote service/method called | `tools/harvest_rpc.py` |

The server rebuilds the `static` schema whenever those files change (content
hash) and never touches `game`.

## Rules the client imposes

* **Column names are attributes.** Rows are read by name (`row.typeName`), so
  the header must match exactly (quoted camelCase in SQL).
* **Column order** is the header order. `categories`, `groups` and `types`
  must have their ID first: for a `sys.Recordset`, constant generation reads
  `i[0]` literally.
* **Row names are constants.** `InsertConstantsFromRowset` turns each row of
  14 tables into `const.<prefix><Name>`: spaces removed, first letter upper-cased
  (`'Asteroid Belt'` → `const.groupAsteroidBelt`). The client dereferences ~350
  of these, so each needs a row with that exact name or a screen dies with
  AttributeError. Some are built dynamically and are listed by hand in
  `gen_seed.py` (`flag{Hi,Med,Lo}Slot0..7`, the resonance attributes).
* **`constants` must not repeat `res:/constants.txt`**: the client's duplicate
  handler catches `'KeyAlreadyExists2'` while the raiser throws
  `'KeyAlreadyExists'`, so a collision kills login.
* **Messages**: rows are merged under `res:/messages.txt`. Any message ID the
  client or server raises that is in neither is an `ErrMessageNotFound`.
  `messageType` decides presentation: `''` silent text, `audio` sound only,
  `notify`/`hint`/`event` a line on screen, `info`/`warning`/`question`/`error`/`fatal`
  a modal box. Passing buttons with any type except warning/question raises.
  Server-raised keys are listed in `gen_seed.py` SERVER_MESSAGES.
* **Hardcoded values**: inventory row indices (`ixItemID`=0 … `ixCustomInfo`=10,
  from the header the client hardcodes twice); owner ranges (≤15 system,
  16..99,999,999 NPC, ≥100,000,000 players); location IDs > 30,000,000 are
  systems/stations; slot flags ordered `SlotFirst ≤ LoSlot0 ≤ … ≤ FixedSlot ≤ SlotLast`.
* **Character creation**: bloodline rows carry the base attributes under
  `attributeName.lower()`, ancestries under `'post' + attributeName`; option
  rows are `(bloodlineID, gender, <stem>ID)` with the face.py stems; option
  names are model directories under `res:/Model/Face/<race>/<bloodline>/<gender>/face/`.
* **Frame limit**: each table travels in one frame, under 1,000,000 bytes.
  The server checks this at startup.

## What is a template default

IDs (except the proven ones above), the category of each group, the group of
each type, enum constant values, message texts, and all of gen_content.py's
authored content are **choices**, marked `TEMPLATE DEFAULT`. They satisfy the
client but are not the original server's data:

* faction and corporation names (keyed by the real faction IDs); 12 schools
  with departments, fields and specialities; skills; one rookie ship per race
* each system's contents - sun, 3-9 planets, moons, belts, one stargate per
  real neighbour placed toward it, a station in every faction system -
  generated deterministically from the system ID
* gear (turrets, miner, shield booster, ammo, drones, pirates), minerals and
  one ore per shipped asteroid model, with what each refines into
* industry: a blueprint per manufactured type (`bptypes`, `blueprint_info`)
  and the minerals one portion takes (`type_materials`)
* the market tree and base prices; one agent per station (`agents`), who is
  also the station's broker
* enum constants: `<family>None` / `<family>Undefined` are 0 and the rest of
  the family counts from 1, so no two names in a family share a value

Every model and icon path is checked against the archives at generation time.
Regenerate with `py -3 -I tools/gen_seed.py > db/static/02_seed.sql` and
`py -3 -I tools/gen_content.py > db/static/04_content.sql`.
