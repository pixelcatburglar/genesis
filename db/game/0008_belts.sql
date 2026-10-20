-- Ore taken from belt asteroids (asteroid IDs are derived from their belt).
CREATE TABLE game.asteroid_depletion (
    "asteroidID" bigint PRIMARY KEY,
    "mined"      bigint NOT NULL DEFAULT 0
);
