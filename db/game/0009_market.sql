-- Player market orders. A sell order holds its goods (the item leaves the
-- hangar), a buy order its money (escrow, paid in advance).
CREATE TABLE game.market_orders (
    "orderID"       integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "typeID"        integer NOT NULL,
    "subTypeID"     integer,
    "ownerID"       integer NOT NULL,
    "charID"        integer NOT NULL,
    "bid"           integer NOT NULL,
    "price"         double precision NOT NULL,
    "volEntered"    integer NOT NULL,
    "volRemaining"  integer NOT NULL,
    "minVolume"     integer NOT NULL DEFAULT 1,
    "range"         integer NOT NULL,
    "stationID"     integer NOT NULL,
    "solarSystemID" integer NOT NULL,
    "constellationID" integer NOT NULL,
    "regionID"      integer NOT NULL,
    "issued"        timestamptz NOT NULL DEFAULT now(),
    "expires"       timestamptz NOT NULL,
    "escrow"        double precision NOT NULL DEFAULT 0
);
CREATE INDEX market_orders_type ON game.market_orders ("typeID", "regionID");
CREATE INDEX market_orders_owner ON game.market_orders ("ownerID");

-- Daily trade history per type and region (market.GetPriceHistory).
CREATE TABLE game.market_history (
    "typeID"     integer NOT NULL,
    "regionID"   integer NOT NULL,
    "day"        date    NOT NULL,
    "lowPrice"   double precision NOT NULL,
    "highPrice"  double precision NOT NULL,
    "closePrice" double precision NOT NULL,
    "volume"     bigint  NOT NULL,
    PRIMARY KEY ("typeID", "regionID", "day")
);
