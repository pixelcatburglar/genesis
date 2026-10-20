-- Industry: blueprint instances, rented factory/research slots and the jobs
-- running in them (factory service and bound object, ui/station/facilities).

-- Per-item blueprint state (factory.GetBlueprintInformation). Items of a
-- blueprint type without a row are originals at level 0.
CREATE TABLE game.blueprints (
    "itemID"            bigint  PRIMARY KEY,
    "copy"              integer NOT NULL DEFAULT 0,
    "mineralLevel"      integer NOT NULL DEFAULT 0,
    "productivityLevel" integer NOT NULL DEFAULT 0,
    "licensedProductionRunsRemaining" integer NOT NULL DEFAULT -1
);

-- A station's slot (flagFactorySlotFirst.. / flagResearchFacilitySlotFirst..)
-- rented to a character or, forCorporateUse, to its corporation.
CREATE TABLE game.factory_slots (
    "stationID"       integer NOT NULL,
    "slotID"          integer NOT NULL,
    "renterID"        integer NOT NULL,
    "forCorporateUse" boolean NOT NULL DEFAULT false,
    "rentedUntil"     timestamptz NOT NULL,
    PRIMARY KEY ("stationID", "slotID")
);

-- The item installed in a slot and what it is doing. Ingredients for every
-- run are taken at install; runs finish one after another.
CREATE TABLE game.factory_jobs (
    "jobID"       integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "stationID"   integer NOT NULL,
    "slotID"      integer NOT NULL,
    "installerID" integer NOT NULL,
    "ownerID"     integer NOT NULL,
    "itemID"      bigint  NOT NULL,
    "activity"    integer NOT NULL,
    "runs"        integer NOT NULL,
    "runsDone"    integer NOT NULL DEFAULT 0,
    "runSeconds"  integer NOT NULL,
    "outputID"    bigint,          -- hangar (station or corp office) results go to
    "started"     timestamptz NOT NULL DEFAULT now(),
    UNIQUE ("stationID", "slotID")
);
