-- Player corporations and everything corpmgr keeps about them
-- (ui/shared/neocom/corporation/*). NPC corporations stay in static.

-- Player corporation IDs: owners >= 100,000,000 are players to the client
-- (sys/cfg.py), and characters start at 100,000,000.
CREATE TABLE game.corporations (
    "corporationID"   integer GENERATED ALWAYS AS IDENTITY (START WITH 140000000) PRIMARY KEY,
    "corporationName" text    NOT NULL,
    "tickerName"      text    NOT NULL,
    "description"     text    NOT NULL DEFAULT '',
    "url"             text    NOT NULL DEFAULT '',
    "taxRate"         double precision NOT NULL DEFAULT 0,
    "ceoID"           integer,
    "creatorID"       integer NOT NULL,
    "stationID"       integer,
    "shares"          integer NOT NULL DEFAULT 1000,
    "balance"         double precision NOT NULL DEFAULT 0,
    -- logo layers: graphicIDs (uix.GetCorpLogo)
    "shape1" integer, "shape2" integer, "shape3" integer,
    "color1" integer, "color2" integer, "color3" integer,
    "memberLimit"          integer NOT NULL DEFAULT 10,
    "allowedMemberRaceIDs" integer NOT NULL DEFAULT 15,
    "created"         timestamptz NOT NULL DEFAULT now(),
    "deleted"         integer NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX corporations_name ON game.corporations (lower("corporationName")) WHERE "deleted" = 0;
CREATE UNIQUE INDEX corporations_ticker ON game.corporations (upper("tickerName")) WHERE "deleted" = 0;

-- Membership data on the character (corpmgr.GetMembers, session corprole).
ALTER TABLE game.characters
    ADD COLUMN "corporationRoles" bigint  NOT NULL DEFAULT 0,
    ADD COLUMN "grantableRoles"   bigint  NOT NULL DEFAULT 0,
    ADD COLUMN "title"            text    NOT NULL DEFAULT '',
    ADD COLUMN "startDateTime"    timestamptz NOT NULL DEFAULT now();

-- corpmgr applications: status is const.crpApplication*.
CREATE TABLE game.corp_applications (
    "corporationID"   integer NOT NULL,
    "characterID"     integer NOT NULL,
    "applicationText" text    NOT NULL DEFAULT '',
    "roles"           bigint  NOT NULL DEFAULT 0,
    "grantableRoles"  bigint  NOT NULL DEFAULT 0,
    "status"          integer NOT NULL,
    "applicationDateTime" timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY ("corporationID", "characterID")
);

-- Shareholders: a character or a corporation holding a corporation's shares.
CREATE TABLE game.shares (
    "ownerID"       integer NOT NULL,
    "corporationID" integer NOT NULL,
    "shares"        integer NOT NULL,
    PRIMARY KEY ("ownerID", "corporationID")
);

-- Votes (votes.py CreateVote): a case with options; each shareholder votes
-- with its shares. voteType is const.vote*; an option's parameter is the
-- subject (new CEO, war target, member to kick, share count).
CREATE TABLE game.vote_cases (
    "voteCaseID"     integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "corporationID"  integer NOT NULL,
    "characterID"    integer NOT NULL,
    "voteType"       integer NOT NULL,
    "voteCaseText"   text    NOT NULL,
    "description"    text    NOT NULL DEFAULT '',
    "startDateTime"  timestamptz NOT NULL,
    "endDateTime"    timestamptz NOT NULL,
    "closed"         integer NOT NULL DEFAULT 0
);
CREATE TABLE game.vote_options (
    "voteCaseID" integer NOT NULL REFERENCES game.vote_cases ON DELETE CASCADE,
    "optionID"   integer NOT NULL,
    "optionText" text    NOT NULL,
    "parameter"  integer NOT NULL DEFAULT 0,
    PRIMARY KEY ("voteCaseID", "optionID")
);
CREATE TABLE game.votes (
    "voteCaseID"  integer NOT NULL REFERENCES game.vote_cases ON DELETE CASCADE,
    "characterID" integer NOT NULL,
    "optionID"    integer NOT NULL,
    "weight"      integer NOT NULL,
    PRIMARY KEY ("voteCaseID", "characterID")
);
-- The winning option of a closed vote, for the CEO to act upon.
CREATE TABLE game.sanctioned_actions (
    "voteCaseID"     integer PRIMARY KEY REFERENCES game.vote_cases ON DELETE CASCADE,
    "corporationID"  integer NOT NULL,
    "voteType"       integer NOT NULL,
    "optionID"       integer NOT NULL,
    "parameter"      integer NOT NULL,
    "inEffect"       integer NOT NULL DEFAULT 0,
    "actedUpon"      integer NOT NULL DEFAULT 0,
    "expires"        timestamptz NOT NULL,
    "timeActedUpon"  timestamptz,
    "timeRescended"  timestamptz
);

CREATE TABLE game.wars (
    "warID"          integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "declaredBy"     integer NOT NULL,
    "against"        integer NOT NULL,
    "timeDeclared"   timestamptz NOT NULL DEFAULT now(),
    "timeFinished"   timestamptz,
    "retracted"      timestamptz,
    "canBlowStuffUp" integer NOT NULL DEFAULT 1
);

-- Station offices: the folder item (typeOfficeFolder) holds the corp hangar.
CREATE TABLE game.offices (
    "officeID"      bigint  PRIMARY KEY,
    "corporationID" integer NOT NULL,
    "stationID"     integer NOT NULL,
    "rentedAt"      timestamptz NOT NULL DEFAULT now()
);

-- Wallet journal (account.GetJournal; util.FmtRef reads refTypeID,
-- ownerID1, ownerID2, argID1).
CREATE TABLE game.journal (
    "refID"      bigint  GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "ownerID"    integer NOT NULL,
    "accountKey" text    NOT NULL DEFAULT 'cash',
    "refTypeID"  integer NOT NULL,
    "transDate"  timestamptz NOT NULL DEFAULT now(),
    "ownerID1"   integer,
    "ownerID2"   integer,
    "argID1"     bigint,
    "amount"     double precision NOT NULL,
    "balance"    double precision NOT NULL,
    "reason"     text NOT NULL DEFAULT ''
);
CREATE INDEX journal_owner ON game.journal ("ownerID", "transDate");
