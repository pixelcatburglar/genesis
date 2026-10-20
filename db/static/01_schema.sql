-- Static data template: the 23 tables the build-1000 client receives at login.
--
-- The client ships no static data. machoNet's AuthenticationRsp carries an
-- `initvals` dict (sys/cfg.py GotInitData), and every value below is one entry.
-- Column NAMES are the wire header and are read by attribute name, so they are
-- quoted to keep their exact case. Column ORDER is the header order: the server
-- sends columns in table order, and categories/groups/types must keep their ID
-- first because cfg.InsertConstantsFromRowset reads i[0] literally for a
-- sys.Recordset.
--
-- Evidence for every column is in docs/client-schema.json
-- (tools/harvest_schema.py): each one is read somewhere in the decompiled client.

CREATE SCHEMA IF NOT EXISTS static;

-- config.BulkData.categories -> cfg.invcategories (Recordset keyed on categoryID)
CREATE TABLE static.categories (
    "categoryID"   integer PRIMARY KEY,
    "categoryName" text    NOT NULL
);

-- config.BulkData.groups -> cfg.invgroups
CREATE TABLE static.groups (
    "groupID"     integer PRIMARY KEY,
    "categoryID"  integer NOT NULL REFERENCES static.categories,
    "groupName"   text    NOT NULL,
    "description" text    NOT NULL DEFAULT '',
    "parentID"    integer REFERENCES static.groups
);

-- config.BulkData.graphics -> cfg.evegraphics
--   icon is a full resource path, e.g. res:/UI/Icon/Mapped/icon01_01.blue,
--   or a corp logo code starting with 'c' (passed to eve.rot.GetInstance).
CREATE TABLE static.graphics (
    "graphicID"   integer PRIMARY KEY,
    "url3D"       text,
    "icon"        text,
    "explosionID" integer
);

-- config.BulkData.types -> cfg.invtypes
CREATE TABLE static.types (
    "typeID"              integer PRIMARY KEY,
    "groupID"             integer          NOT NULL REFERENCES static.groups,
    "typeName"            text             NOT NULL,
    "description"         text             NOT NULL DEFAULT '',
    "graphicID"           integer          REFERENCES static.graphics,
    "volume"              double precision NOT NULL DEFAULT 0,
    "capacity"            double precision NOT NULL DEFAULT 0,
    "portionSize"         integer          NOT NULL DEFAULT 1,
    "chanceOfDuplicating" double precision NOT NULL DEFAULT 0,
    "marketGroupID"       integer
);

-- config.BulkData.bptypes -> cfg.invbptypes. Denormalised in this build: the
-- client reads blueprintTypeName/description/basePrice off the blueprint row.
CREATE TABLE static.bptypes (
    "blueprintTypeID"            integer PRIMARY KEY REFERENCES static.types,
    "blueprintTypeName"          text             NOT NULL,
    "description"                text             NOT NULL DEFAULT '',
    "parentBlueprintTypeID"      integer          REFERENCES static.types,
    "productTypeID"              integer          NOT NULL REFERENCES static.types,
    "techLevel"                  integer          NOT NULL DEFAULT 1,
    "chanceOfReverseEngineering" double precision NOT NULL DEFAULT 0,
    "basePrice"                  double precision NOT NULL DEFAULT 0
);

-- config.Units -> constants 'unit*'
CREATE TABLE static.units (
    "unitID"      integer PRIMARY KEY,
    "unitName"    text NOT NULL,
    -- infosvc FormatValue suffix (config.GetUnits)
    "displayName" text NOT NULL DEFAULT ''
);

-- config.BulkData.dgmattribs -> cfg.dgmattribs (IndexRowset on attributeID).
-- attributeName mangles to const.attribute<Name> (first letter upper-cased).
CREATE TABLE static.dgmattribs (
    "attributeID"   integer PRIMARY KEY,
    "attributeName" text             NOT NULL,
    "displayName"   text             NOT NULL DEFAULT '',
    "defaultValue"  double precision NOT NULL DEFAULT 0,
    "published"     integer          NOT NULL DEFAULT 0,
    "unitID"        integer          REFERENCES static.units,
    "graphicID"     integer          REFERENCES static.graphics,
    -- dogmaIM.GetAttributeTypes metadata (environment/godma.py InitializeData):
    -- a charged attribute names its recharge-time and maximum attributes.
    "attributeCategory"    integer NOT NULL DEFAULT 0,
    "chargeRechargeTimeID" integer,
    "maxAttributeID"       integer,
    "highIsGood"           integer NOT NULL DEFAULT 1,
    "stackable"            integer NOT NULL DEFAULT 1
);

-- config.BulkData.dgmeffects -> cfg.dgmeffects (IndexRowset on effectID)
CREATE TABLE static.dgmeffects (
    "effectID"                 integer PRIMARY KEY,
    "effectName"               text    NOT NULL,
    "displayName"              text    NOT NULL DEFAULT '',
    "description"              text    NOT NULL DEFAULT '',
    "effectCategory"           integer NOT NULL DEFAULT 0,
    "isOffensive"              integer NOT NULL DEFAULT 0,
    "trackingSpeedAttributeID" integer REFERENCES static.dgmattribs,
    "rangeAttributeID"         integer REFERENCES static.dgmattribs,
    "falloffAttributeID"       integer REFERENCES static.dgmattribs,
    "dischargeAttributeID"     integer REFERENCES static.dgmattribs,
    "durationAttributeID"      integer REFERENCES static.dgmattribs,
    "graphicID"                integer REFERENCES static.graphics
);

-- config.BulkData.dgmtypeattribs -> cfg.dgmtypeattribs (FilterRowset on typeID)
CREATE TABLE static.dgmtypeattribs (
    "typeID"      integer          NOT NULL REFERENCES static.types,
    "attributeID" integer          NOT NULL REFERENCES static.dgmattribs,
    "value"       double precision NOT NULL,
    PRIMARY KEY ("typeID", "attributeID")
);

-- config.BulkData.dgmtypeeffects -> cfg.dgmtypeeffects (FilterRowset on typeID)
CREATE TABLE static.dgmtypeeffects (
    "typeID"    integer NOT NULL REFERENCES static.types,
    "effectID"  integer NOT NULL REFERENCES static.dgmeffects,
    "isDefault" integer NOT NULL DEFAULT 0,
    PRIMARY KEY ("typeID", "effectID")
);

-- config.BulkData.shiptypes -> cfg.shiptypes (IndexRowset on shipTypeID).
-- The 2003 hull model: every ship has fixed built-in module types, read by
-- godma.py's shipSlotNames and infosvc ("Fitted Modules").
CREATE TABLE static.shiptypes (
    "shipTypeID"       integer PRIMARY KEY REFERENCES static.types,
    "powerCoreTypeID"  integer REFERENCES static.types,
    "capacitorTypeID"  integer REFERENCES static.types,
    "shieldTypeID"     integer REFERENCES static.types,
    "propulsionTypeID" integer REFERENCES static.types,
    "sensorTypeID"     integer REFERENCES static.types,
    "armorTypeID"      integer REFERENCES static.types,
    "computerTypeID"   integer REFERENCES static.types,
    "weaponTypeID"     integer REFERENCES static.types,
    "miningTypeID"     integer REFERENCES static.types
);

-- config.BulkData.owners -> cfg.eveowners. ID ranges are client-hardcoded
-- (cfg.py EveOwners): <= 15 system, 16..99,999,999 NPC, >= 100,000,000 players.
-- groupID is NOT a column: EveOwners derives it through typeID.
CREATE TABLE static.owners (
    "ownerID"   integer PRIMARY KEY,
    "ownerName" text    NOT NULL,
    "typeID"    integer NOT NULL REFERENCES static.types
);

-- config.BulkData.locations -> cfg.evelocations. IDs above 30,000,000 are
-- solar systems/stations (bookmarks.py).
CREATE TABLE static.locations (
    "locationID"   integer PRIMARY KEY,
    "locationName" text NOT NULL
);

-- config.BulkData.messages -> cfg.evemessages. Header hardcoded in cfg.py
-- (msgheader). Rows here are merged over res:/messages.txt, which ships with
-- the client; every message the server raises must exist in one of them.
CREATE TABLE static.messages (
    "messageID"   text PRIMARY KEY,
    "messageType" text NOT NULL,
    "messageText" text NOT NULL,
    "urlAudio"    text,
    "urlIcon"     text,
    "locale"      text
);

-- config.BulkData.constants -> const.<constantID> = int(constantValue).
-- Must stay disjoint from res:/constants.txt: a collision escapes the client's
-- duplicate handler (it catches 'KeyAlreadyExists2' but the raiser throws
-- 'KeyAlreadyExists') and kills login.
CREATE TABLE static.constants (
    "constantID"    text    PRIMARY KEY,
    "constantValue" integer NOT NULL
);

-- config.BulkData.billtypes -> constants 'billType*'
CREATE TABLE static.billtypes (
    "billTypeID"   integer PRIMARY KEY,
    "billTypeName" text NOT NULL
);

-- config.Roles / Flags / Races / Bloodlines / Statistics / ChannelTypes /
-- EncyclopediaTypes: util.Rowset objects whose rows generate const.<prefix><Name>.
CREATE TABLE static.roles (
    "roleID"   bigint PRIMARY KEY,
    "roleName" text NOT NULL
);

CREATE TABLE static.flags (
    "flagID"   integer PRIMARY KEY,
    "flagName" text NOT NULL
);

CREATE TABLE static.races (
    "raceID"   integer PRIMARY KEY,
    "raceName" text NOT NULL
);

CREATE TABLE static.bloodlines (
    "bloodlineID"   integer PRIMARY KEY,
    "bloodlineName" text    NOT NULL,
    "raceID"        integer NOT NULL REFERENCES static.races
);

CREATE TABLE static.statistics (
    "statisticID"   integer PRIMARY KEY,
    "statisticName" text NOT NULL
);

CREATE TABLE static.channeltypes (
    "channelTypeID"   integer PRIMARY KEY,
    "channelTypeName" text NOT NULL
);

CREATE TABLE static.encyclopediatypes (
    "encyclopediaTypeID"   integer PRIMARY KEY,
    "encyclopediaTypeName" text NOT NULL
);

-- Which table backs each initvals key, and how it is wrapped on the wire:
--   'bulk'   -> (header, rows) tuple    (read via GetCachedObject()[0]/[1])
--   'rowset' -> util.Rowset instance    (iterated as row objects)
CREATE TABLE static.initvals (
    "key"     text PRIMARY KEY,
    "tbl"     text NOT NULL,
    "shape"   text NOT NULL CHECK ("shape" IN ('bulk', 'rowset')),
    "orderBy" text NOT NULL
);

INSERT INTO static.initvals VALUES
    ('config.BulkData.constants',      'constants',         'bulk',   '"constantID"'),
    ('config.BulkData.messages',       'messages',          'bulk',   '"messageID"'),
    ('config.BulkData.graphics',       'graphics',          'bulk',   '"graphicID"'),
    ('config.BulkData.owners',         'owners',            'bulk',   '"ownerID"'),
    ('config.BulkData.locations',      'locations',         'bulk',   '"locationID"'),
    ('config.BulkData.categories',     'categories',        'bulk',   '"categoryID"'),
    ('config.BulkData.groups',         'groups',            'bulk',   '"groupID"'),
    ('config.BulkData.types',          'types',             'bulk',   '"typeID"'),
    ('config.BulkData.bptypes',        'bptypes',           'bulk',   '"blueprintTypeID"'),
    ('config.BulkData.dgmattribs',     'dgmattribs',        'bulk',   '"attributeID"'),
    ('config.BulkData.dgmeffects',     'dgmeffects',        'bulk',   '"effectID"'),
    ('config.BulkData.dgmtypeattribs', 'dgmtypeattribs',    'bulk',   '"typeID", "attributeID"'),
    ('config.BulkData.dgmtypeeffects', 'dgmtypeeffects',    'bulk',   '"typeID", "effectID"'),
    ('config.BulkData.shiptypes',      'shiptypes',         'bulk',   '"shipTypeID"'),
    ('config.BulkData.billtypes',      'billtypes',         'bulk',   '"billTypeID"'),
    ('config.Roles',                   'roles',             'rowset', '"roleID"'),
    ('config.Flags',                   'flags',             'rowset', '"flagID"'),
    ('config.Races',                   'races',             'rowset', '"raceID"'),
    ('config.Bloodlines',              'bloodlines',        'rowset', '"bloodlineID"'),
    ('config.Statistics',              'statistics',        'rowset', '"statisticID"'),
    ('config.Units',                   'units',             'rowset', '"unitID"'),
    ('config.ChannelTypes',            'channeltypes',      'rowset', '"channelTypeID"'),
    ('config.EncyclopediaTypes',       'encyclopediatypes', 'rowset', '"encyclopediaTypeID"');
