-- Per-character state behind the neocom services: bookmarks, mail, petitions,
-- forums, standings, bounties, travel history, bills, ship paint.

-- bookmark.GetBookmarks rows (ui/shared/neocom/bookmarks.py, addressbook.py).
-- itemID/typeID name what was bookmarked (a celestial, station, ship or, for
-- the address book, a character); x/y/z is a spot in space; memo is
-- 'label\tnote' (bookmarks.Zip).
CREATE TABLE game.bookmarks (
    "bookmarkID" integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "ownerID"    integer NOT NULL,
    "itemID"     bigint,
    "typeID"     integer NOT NULL,
    "flag"       integer,
    "memo"       text    NOT NULL DEFAULT '',
    "created"    timestamptz NOT NULL DEFAULT now(),
    "x"          double precision,
    "y"          double precision,
    "z"          double precision,
    "locationID" bigint
);
CREATE INDEX bookmarks_owner ON game.bookmarks ("ownerID");

-- Voucher items (groupVoucher): a bookmark copy carried as an item.
CREATE TABLE game.vouchers (
    "itemID"      bigint PRIMARY KEY REFERENCES game.items ON DELETE CASCADE,
    "description" text    NOT NULL,
    "typeID"      integer,
    "subTypeID"   integer,
    -- the bookmark it restores (AddBookmarkFromVoucher)
    "bmItemID"    bigint,
    "bmTypeID"    integer,
    "bmX"         double precision,
    "bmY"         double precision,
    "bmZ"         double precision,
    "bmLocationID" bigint
);

-- pager: EVE mail (ui/shared/neocom/inbox.py). charownerID is the sender.
CREATE TABLE game.pages (
    "pageID"      integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "receiverID"  integer NOT NULL,
    "charownerID" integer NOT NULL,
    "subject"     text    NOT NULL,
    "text"        text    NOT NULL,
    "created"     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX pages_receiver ON game.pages ("receiverID");

-- petitioner (ui/shared/neocom/petition.py).
CREATE TABLE game.petitions (
    "petitionID"   integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "petitionerID" integer NOT NULL,
    "categoryID"   integer NOT NULL,
    "subject"      text    NOT NULL,
    "petition"     text    NOT NULL,
    "properties"   jsonb,
    "queueID"      integer NOT NULL DEFAULT 1,
    "claimedBy"    integer,
    "closed"       integer NOT NULL DEFAULT 0,
    "deleted"      integer NOT NULL DEFAULT 0,
    "updated"      integer NOT NULL DEFAULT 0,
    "createDate"   timestamptz NOT NULL DEFAULT now(),
    "touchDate"    timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE game.petition_messages (
    "messageID"  integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "petitionID" integer NOT NULL REFERENCES game.petitions ON DELETE CASCADE,
    "senderID"   integer NOT NULL,
    "text"       text    NOT NULL,
    "comment"    integer NOT NULL DEFAULT 0,
    "readBy"     integer[] NOT NULL DEFAULT '{}',
    "sentDate"   timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE game.petition_log (
    "logID"       integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "petitionID"  integer NOT NULL REFERENCES game.petitions ON DELETE CASCADE,
    "characterID" integer,
    "userName"    text    NOT NULL DEFAULT '',
    "eventText"   text    NOT NULL,
    "logDateTime" timestamptz NOT NULL DEFAULT now()
);

-- newsmgr forums (ui/station/news). Channels are seeded below.
CREATE TABLE game.news_channels (
    "channelID"   integer PRIMARY KEY,
    "title"       text NOT NULL,
    "description" text NOT NULL
);
CREATE TABLE game.news_threads (
    "threadID"    integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "channelID"   integer NOT NULL REFERENCES game.news_channels,
    "authorID"    integer NOT NULL,
    "title"       text    NOT NULL,
    "description" text    NOT NULL,
    "reads"       integer NOT NULL DEFAULT 0,
    "created"     timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE game.news_responses (
    "responseID" integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "threadID"   integer NOT NULL REFERENCES game.news_threads ON DELETE CASCADE,
    "authorID"   integer NOT NULL,
    "text"       text    NOT NULL,
    "created"    timestamptz NOT NULL DEFAULT now()
);
-- TEMPLATE DEFAULT forum channels.
INSERT INTO game.news_channels VALUES
    (1, 'General', 'General discussion among capsuleers.'),
    (2, 'Trade', 'Buying, selling and hauling.'),
    (3, 'Recruitment', 'Corporations looking for members, and pilots looking for corporations.');

-- Standings: a character's or corporation's standing towards another owner
-- (standingmgr private standings, corpmgr corporation standings, and the
-- NPC standings shown on the character sheet).
CREATE TABLE game.standings (
    "fromID"   integer NOT NULL,
    "toID"     integer NOT NULL,
    "standing" double precision NOT NULL DEFAULT 0,
    "notes"    text NOT NULL DEFAULT '',
    PRIMARY KEY ("fromID", "toID")
);

ALTER TABLE game.characters ADD COLUMN "bounty" double precision NOT NULL DEFAULT 0;

-- character.GetSolarSystemVisits (map STARMODE_VISITED).
CREATE TABLE game.system_visits (
    "characterID"   integer NOT NULL,
    "solarSystemID" integer NOT NULL,
    "visits"        integer NOT NULL DEFAULT 1,
    "lastDateTime"  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY ("characterID", "solarSystemID")
);

-- character.GetHistory(kind): recent per-system events (jumps, kills) the
-- map colours systems by.
CREATE TABLE game.system_events (
    "solarSystemID" integer NOT NULL,
    "kind"          integer NOT NULL,
    "at"            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX system_events_at ON game.system_events ("kind", "at");

-- billMgr (ui/shared/neocom/bills.py).
CREATE TABLE game.bills (
    "billID"      integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    "billTypeID"  integer NOT NULL,
    "debtorID"    integer NOT NULL,
    "creditorID"  integer NOT NULL,
    "amount"      double precision NOT NULL,
    "dueDateTime" timestamptz NOT NULL,
    "interest"    double precision NOT NULL DEFAULT 0,
    "externalID"  integer,
    "paid"        integer NOT NULL DEFAULT 0
);

-- shipSvc.GetShipAppearance / station.Paintshop_SetShipAppearance.
CREATE TABLE game.ship_appearance (
    "shipID"      bigint PRIMARY KEY,
    "layer1Shape" integer, "layer1Color" integer,
    "layer2Shape" integer, "layer2Color" integer,
    "layer3Shape" integer, "layer3Color" integer,
    "shipColor1"  integer, "shipColor2"  integer,
    "tag"         text,    "tagColor"    integer, "tagFont" integer
);
