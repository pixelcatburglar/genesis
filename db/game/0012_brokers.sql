-- Brokers (bound broker[stationID], ui/station/brokers/base_brokers.py): a
-- pilot signs up with one broker per station, who then lists the items in
-- the pilot's hangar there on the market as the pilot's rules allow.
CREATE TABLE game.broker_clients (
    "stationID" integer NOT NULL,
    "clientID"  integer NOT NULL,
    "brokerID"  integer NOT NULL,
    PRIMARY KEY ("stationID", "clientID")
);

-- One of categoryID / groupID / typeID is set (the others 0); the most
-- specific rule matching an item decides.
CREATE TABLE game.broker_rules (
    "brokerID"   integer NOT NULL,
    "clientID"   integer NOT NULL,
    "categoryID" integer NOT NULL DEFAULT 0,
    "groupID"    integer NOT NULL DEFAULT 0,
    "typeID"     integer NOT NULL DEFAULT 0,
    "allowed"    integer NOT NULL,
    PRIMARY KEY ("brokerID", "clientID", "categoryID", "groupID", "typeID")
);
