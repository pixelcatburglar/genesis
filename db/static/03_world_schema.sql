-- World and character-creation tables (static schema, rebuilt with its content).
-- Map rows come from the universe the client ships in res:/UI/Shared/Maps/cachefile.blue
-- (tools/gen_content.py); celestials, stations and creation content are template data.

-- config.GetMapObjects rows, header exactly as recorded in the shipped map cache:
-- groupID, typeID, itemID, itemName, locationID, orbitID, connector, x, y, z,
-- xMin, xMax, yMin, yMax, zMin, zMax, luminosity, factionID, security
CREATE TABLE static.map_objects (
    "groupID"    integer NOT NULL,
    "typeID"     integer NOT NULL,
    "itemID"     integer PRIMARY KEY,
    "itemName"   text    NOT NULL,
    "locationID" integer,
    "orbitID"    integer,
    "connector"  integer NOT NULL DEFAULT 0,
    "x"          double precision NOT NULL,
    "y"          double precision NOT NULL,
    "z"          double precision NOT NULL,
    "xMin"       double precision,
    "xMax"       double precision,
    "yMin"       double precision,
    "yMax"       double precision,
    "zMin"       double precision,
    "zMax"       double precision,
    "luminosity" double precision,
    "factionID"  integer,
    "security"   double precision
);
CREATE INDEX map_objects_location ON static.map_objects ("locationID");

-- Gate graph, both directions (the shipped cache lists each system's neighbours).
CREATE TABLE static.map_jumps (
    "fromSolarSystemID" integer NOT NULL,
    "toSolarSystemID"   integer NOT NULL,
    PRIMARY KEY ("fromSolarSystemID", "toSolarSystemID")
);

-- config.GetMapLandmarks (shipped cache 'landmarks').
CREATE TABLE static.map_landmarks (
    "landmarkID"   integer PRIMARY KEY,
    "landmarkName" text    NOT NULL,
    "description"  text    NOT NULL,
    "radius"       double precision NOT NULL,
    "x"            double precision NOT NULL,
    "y"            double precision NOT NULL,
    "z"            double precision NOT NULL,
    "importance"   integer NOT NULL
);

-- Everything inside a solar system: sun, planets, moons, belts, stargates, stations.
-- Positions are metres relative to the system's sun.
CREATE TABLE static.celestials (
    "itemID"          integer PRIMARY KEY,
    "typeID"          integer NOT NULL,
    "groupID"         integer NOT NULL,
    "solarSystemID"   integer NOT NULL,
    "constellationID" integer NOT NULL,
    "regionID"        integer NOT NULL,
    "orbitID"         integer,
    "x"               double precision NOT NULL,
    "y"               double precision NOT NULL,
    "z"               double precision NOT NULL,
    "radius"          double precision NOT NULL,
    "itemName"        text    NOT NULL,
    "celestialIndex"  integer,
    "orbitIndex"      integer
);
CREATE INDEX celestials_system ON static.celestials ("solarSystemID");

-- Stargate itemID -> destination stargate itemID.
CREATE TABLE static.stargates (
    "stargateID"    integer PRIMARY KEY,
    "destinationID" integer NOT NULL
);

CREATE TABLE static.station_types (
    "stationTypeID"   integer PRIMARY KEY,
    -- eve.stationItem.hangarGraphicID -> evegraphics url3D = the hangar scene
    "hangarGraphicID" integer NOT NULL,
    "raceID"          integer
);

CREATE TABLE static.stations (
    "stationID"       integer PRIMARY KEY,
    "stationTypeID"   integer NOT NULL,
    "corporationID"   integer NOT NULL,
    "solarSystemID"   integer NOT NULL,
    "constellationID" integer NOT NULL,
    "regionID"        integer NOT NULL,
    -- "<location> - <owner> <kind>": charsel splits on ' - '
    "stationName"     text    NOT NULL,
    -- const.stationService* bits (eve.stationItem.serviceMask)
    "serviceMask"     integer NOT NULL,
    "x"               double precision NOT NULL,
    "y"               double precision NOT NULL,
    "z"               double precision NOT NULL
);
CREATE INDEX stations_system ON static.stations ("solarSystemID");

-- NPC corporations (owners 1,000,000+) and factions (owners 500,000+).
CREATE TABLE static.npc_corporations (
    "corporationID"   integer PRIMARY KEY,
    "corporationName" text    NOT NULL,
    "tickerName"      text    NOT NULL,
    "factionID"       integer,
    "description"     text    NOT NULL DEFAULT '',
    "stationID"       integer
);

CREATE TABLE static.factions (
    "factionID"   integer PRIMARY KEY,
    "factionName" text NOT NULL,
    "description" text NOT NULL DEFAULT '',
    "raceIDs"     integer
);

-- ------------------------------------------------------------------ creation
-- character.GetCharCreationInfo. Field names are what ui/login/{steps,
-- charcreationbase,ccutil}.py read: bloodlines carry the five base attributes
-- under attributeName.lower(), ancestries carry 'post' + attributeName.

ALTER TABLE static.races
    ADD COLUMN "description"  text    NOT NULL DEFAULT '',
    ADD COLUMN "graphicID"    integer,
    ADD COLUMN "skillTypeID1" integer;

ALTER TABLE static.bloodlines
    ADD COLUMN "description"       text    NOT NULL DEFAULT '',
    ADD COLUMN "maleDescription"   text    NOT NULL DEFAULT '',
    ADD COLUMN "femaleDescription" text    NOT NULL DEFAULT '',
    ADD COLUMN "graphicID"         integer,
    ADD COLUMN "skillTypeID1"      integer,
    ADD COLUMN "skillTypeID2"      integer,
    ADD COLUMN "charisma"          integer NOT NULL DEFAULT 0,
    ADD COLUMN "intelligence"      integer NOT NULL DEFAULT 0,
    ADD COLUMN "memory"            integer NOT NULL DEFAULT 0,
    ADD COLUMN "perception"        integer NOT NULL DEFAULT 0,
    ADD COLUMN "willpower"         integer NOT NULL DEFAULT 0,
    ADD COLUMN "shipTypeID"        integer,
    ADD COLUMN "corporationID"     integer;

-- attributeID must equal const.chrattr*: CreateCharacter indexes the modifier
-- dict by those constants (charcreationbase.py).
CREATE TABLE static.cc_attributes (
    "attributeID"   integer PRIMARY KEY,
    "attributeName" text    NOT NULL,
    "description"   text    NOT NULL,
    "graphicID"     integer
);

CREATE TABLE static.cc_ancestries (
    "ancestryID"     integer PRIMARY KEY,
    "ancestryName"   text    NOT NULL,
    "bloodlineID"    integer NOT NULL,
    "description"    text    NOT NULL,
    "graphicID"      integer,
    "postCharisma"     integer NOT NULL DEFAULT 0,
    "postIntelligence" integer NOT NULL DEFAULT 0,
    "postMemory"       integer NOT NULL DEFAULT 0,
    "postPerception"   integer NOT NULL DEFAULT 0,
    "postWillpower"    integer NOT NULL DEFAULT 0,
    "skillTypeID1"   integer,
    "skillTypeID2"   integer
);

CREATE TABLE static.cc_schools (
    "schoolID"      integer PRIMARY KEY,
    "raceID"        integer NOT NULL,
    "schoolName"    text    NOT NULL,
    "description"   text    NOT NULL,
    "graphicID"     integer,
    "corporationID" integer NOT NULL,
    "stationID"     integer NOT NULL
);

CREATE TABLE static.cc_departments (
    "departmentID"   integer PRIMARY KEY,
    "schoolID"       integer NOT NULL,
    "departmentName" text    NOT NULL,
    "description"    text    NOT NULL,
    "graphicID"      integer,
    "skillTypeID1"   integer,
    "skillTypeID2"   integer,
    "skillTypeID3"   integer
);

CREATE TABLE static.cc_fields (
    "fieldID"      integer PRIMARY KEY,
    "departmentID" integer NOT NULL,
    "fieldName"    text    NOT NULL,
    "description"  text    NOT NULL,
    "graphicID"    integer,
    "skillTypeID1" integer,
    "skillTypeID2" integer,
    "typeID"       integer,
    "typeQuantity" integer
);

CREATE TABLE static.cc_specialities (
    "specialityID"   integer PRIMARY KEY,
    "fieldID"        integer NOT NULL,
    "specialityName" text    NOT NULL,
    "description"    text    NOT NULL,
    "graphicID"      integer,
    "skillTypeID1"   integer,
    "skillTypeID2"   integer,
    "typeID"         integer,
    "typeQuantity"   integer
);

-- Appearance. Names are asset directories:
--   res:/Model/Face/<Race>/<Bloodline>/<gender>/face/<family dir>/<name>/default.blue
-- lights/backgrounds under res:/Model/Face/SceneOptions/{Lights,Background}/<name>/
-- `family` is the face.py allOptions key (skins, eyes, ..., lights, backgrounds).
CREATE TABLE static.cc_appearance_names (
    "family" text    NOT NULL,
    "id"     integer NOT NULL,
    "name"   text    NOT NULL,
    PRIMARY KEY ("family", "id")
);

CREATE TABLE static.cc_appearance_options (
    "family"      text    NOT NULL,
    "bloodlineID" integer NOT NULL,
    "gender"      integer NOT NULL,
    "id"          integer NOT NULL,
    PRIMARY KEY ("family", "bloodlineID", "gender", "id")
);

-- ------------------------------------------------------------- services
-- station.Paintshop_GetLayerShapes / GetLayerColors: (graphicID, path[, label]).
CREATE TABLE static.logo_library (
    "graphicID" integer PRIMARY KEY,
    "kind"      text    NOT NULL,   -- 'shape' | 'color'
    "path"      text    NOT NULL,
    "label"     text
);

-- petitioner categories, the extra fields each asks for, and the queues
-- petitionees work (ui/shared/neocom/petition.py).
CREATE TABLE static.petition_queues (
    "queueID"     integer PRIMARY KEY,
    "queueName"   text    NOT NULL,
    "escalatesTo" integer
);
CREATE TABLE static.petition_categories (
    "categoryID"  integer PRIMARY KEY,
    "displayName" text    NOT NULL,
    "description" text    NOT NULL,
    "queueID"     integer NOT NULL
);
CREATE TABLE static.petition_properties (
    "categoryID"   integer NOT NULL,
    "propertyName" text    NOT NULL,
    PRIMARY KEY ("categoryID", "propertyName")
);

-- What 100 units (portionSize) of a type reprocess into (reprocessing service,
-- factory.GetMineralCompositionOfItemType).
CREATE TABLE static.type_materials (
    "typeID"         integer NOT NULL,
    "materialTypeID" integer NOT NULL,
    "quantity"       integer NOT NULL,
    PRIMARY KEY ("typeID", "materialTypeID")
);

-- market.GetMarketGroups: the tree, children under parentGroupID (None = root).
CREATE TABLE static.market_groups (
    "marketGroupID"   integer PRIMARY KEY,
    "parentGroupID"   integer,
    "marketGroupName" text    NOT NULL,
    "description"     text    NOT NULL DEFAULT '',
    "graphicID"       integer,
    "hasTypes"        integer NOT NULL DEFAULT 0
);

-- What the stations' standing NPC orders trade a type at.
CREATE TABLE static.type_prices (
    "typeID"    integer PRIMARY KEY,
    "basePrice" double precision NOT NULL
);

-- newBreedAgents: NPC characters handing out missions at their station.
CREATE TABLE static.agents (
    "agentID"       integer PRIMARY KEY,
    "agentName"     text    NOT NULL,
    "corporationID" integer NOT NULL,
    "stationID"     integer NOT NULL,
    "raceID"        integer NOT NULL,
    "bloodlineID"   integer NOT NULL,
    "level"         integer NOT NULL
);
CREATE INDEX agents_station ON static.agents ("stationID");

-- factory.GetBlueprintAttributes: how long a blueprint's jobs take (seconds
-- per run) and the share of extra minerals a run wastes.
CREATE TABLE static.blueprint_info (
    "blueprintTypeID"          integer PRIMARY KEY,
    "manufacturingTime"        integer NOT NULL,
    "wastageFactor"            double precision NOT NULL,
    "researchMineralTime"      integer NOT NULL,
    "researchCopyTime"         integer NOT NULL,
    "researchProductivityTime" integer NOT NULL
);
