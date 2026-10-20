-- The skill a character trains (one at a time; the item's flag is
-- flagSkillInTraining while this row exists).
CREATE TABLE game.skill_training (
    "characterID" integer PRIMARY KEY,
    "itemID"      bigint  NOT NULL,
    "startTime"   timestamptz NOT NULL DEFAULT now(),
    "endTime"     timestamptz NOT NULL,
    "startSP"     double precision NOT NULL,
    "targetSP"    double precision NOT NULL,
    "targetLevel" integer NOT NULL
);
CREATE INDEX skill_training_end ON game.skill_training ("endTime");
