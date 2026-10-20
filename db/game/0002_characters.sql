-- Characters and items. No foreign keys into `static`: the static schema is
-- dropped and rebuilt whenever its content changes.

-- Player owner IDs start at 100,000,000: the client treats 16..99,999,999 as
-- NPC owners and <= 15 as system owners (sys/cfg.py EveOwners).
CREATE TABLE game.characters (
    "characterID"     integer GENERATED ALWAYS AS IDENTITY (START WITH 100000000) PRIMARY KEY,
    "userID"          integer     NOT NULL REFERENCES game.accounts,
    "characterName"   text        NOT NULL,
    "bloodlineID"     integer     NOT NULL,
    "raceID"          integer     NOT NULL,
    "gender"          integer     NOT NULL,
    "ancestryID"      integer     NOT NULL,
    "schoolID"        integer     NOT NULL,
    "departmentID"    integer     NOT NULL,
    "fieldID"         integer     NOT NULL,
    "specialityID"    integer     NOT NULL,
    "charisma"        integer     NOT NULL,
    "intelligence"    integer     NOT NULL,
    "memory"          integer     NOT NULL,
    "perception"      integer     NOT NULL,
    "willpower"       integer     NOT NULL,
    "corporationID"   integer     NOT NULL,
    "stationID"       integer,
    "solarSystemID"   integer     NOT NULL,
    "constellationID" integer     NOT NULL,
    "regionID"        integer     NOT NULL,
    "shipID"          bigint,
    "balance"         double precision NOT NULL DEFAULT 0,
    "securityStatus"  double precision NOT NULL DEFAULT 0,
    "homeStationID"   integer,
    "created"         timestamptz NOT NULL DEFAULT now(),
    "deleted"         timestamptz
);
CREATE UNIQUE INDEX characters_name ON game.characters (lower("characterName")) WHERE "deleted" IS NULL;
CREATE INDEX characters_user ON game.characters ("userID");

-- The appearance dict sent in CreateCharacter (face.py AsDict): option IDs
-- (skinID, hairID, ...), morph values, camPos/headRotation/eyeRotation 1..3.
CREATE TABLE game.character_appearance (
    "characterID" integer NOT NULL REFERENCES game.characters,
    "key"         text    NOT NULL,
    "intValue"    bigint,
    "floatValue"  double precision,
    "textValue"   text,
    PRIMARY KEY ("characterID", "key")
);

-- Inventory items. Row shape follows the client's hardcoded inventory header
-- (itemID, typeID, ownerID, locationID, flag, contraband, singleton, quantity,
-- groupID, categoryID, customInfo); group/category are joined from static.
CREATE TABLE game.items (
    "itemID"     bigint GENERATED ALWAYS AS IDENTITY (START WITH 150000000) PRIMARY KEY,
    "typeID"     integer NOT NULL,
    "ownerID"    integer NOT NULL,
    "locationID" bigint  NOT NULL,
    "flag"       integer NOT NULL DEFAULT 0,
    "contraband" integer NOT NULL DEFAULT 0,
    "singleton"  integer NOT NULL DEFAULT 0,
    "quantity"   integer NOT NULL DEFAULT 1,
    "customInfo" text,
    "itemName"   text
);
CREATE INDEX items_location ON game.items ("locationID");
CREATE INDEX items_owner ON game.items ("ownerID");

-- Per-item dogma attribute overrides (skill levels, skill points, ...).
CREATE TABLE game.item_attributes (
    "itemID"      bigint  NOT NULL REFERENCES game.items ON DELETE CASCADE,
    "attributeID" integer NOT NULL,
    "value"       double precision NOT NULL,
    PRIMARY KEY ("itemID", "attributeID")
);
