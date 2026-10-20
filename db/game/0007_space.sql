-- Player objects left in space: jettisoned cargo containers, abandoned
-- ships, launched drones. The item's locationID is the solar system; this
-- is where in it.
CREATE TABLE game.space_objects (
    "itemID" bigint PRIMARY KEY REFERENCES game.items ON DELETE CASCADE,
    "x"      double precision NOT NULL,
    "y"      double precision NOT NULL,
    "z"      double precision NOT NULL
);

-- Where a character's ship is in space, so logging in out there puts it back.
ALTER TABLE game.characters ADD COLUMN "spaceX" double precision,
                            ADD COLUMN "spaceY" double precision,
                            ADD COLUMN "spaceZ" double precision;
