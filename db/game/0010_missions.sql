-- Agent missions (newBreedAgents). kind: 'courier' (move cargo to a
-- station) or 'mining' (bring ore to the agent).
CREATE TABLE game.agent_missions (
    "missionID"     integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "agentID"       integer NOT NULL,
    "characterID"   integer NOT NULL,
    "kind"          text    NOT NULL,
    "status"        text    NOT NULL,          -- offered | accepted | done
    "typeID"        integer NOT NULL,
    "quantity"      integer NOT NULL,
    "destinationID" integer NOT NULL,
    "reward"        double precision NOT NULL,
    "created"       timestamptz NOT NULL DEFAULT now(),
    "expires"       timestamptz NOT NULL
);
CREATE INDEX agent_missions_char ON game.agent_missions ("characterID", "status");

-- Courier contracts between players (missionMgr): items packed at a station
-- for someone to carry to another for a reward against collateral.
CREATE TABLE game.courier_missions (
    "missionID"     integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "issuerID"      integer NOT NULL,
    "acceptorID"    integer,
    "startStationID" integer NOT NULL,
    "destinationID" integer NOT NULL,
    "packageID"     bigint,
    "volume"        double precision NOT NULL DEFAULT 0,
    "reward"        double precision NOT NULL DEFAULT 0,
    "collateral"    double precision NOT NULL DEFAULT 0,
    "acceptFee"     double precision NOT NULL DEFAULT 0,
    "status"        text    NOT NULL DEFAULT 'open',  -- open | accepted | delivered | cancelled
    "created"       timestamptz NOT NULL DEFAULT now(),
    "expires"       timestamptz NOT NULL
);
