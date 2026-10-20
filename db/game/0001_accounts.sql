-- Player accounts. The client sends userName/password in AuthenticationReq.
CREATE SCHEMA IF NOT EXISTS game;

CREATE TABLE game.accounts (
    "userID"    integer GENERATED ALWAYS AS IDENTITY (START WITH 1) PRIMARY KEY,
    "userName"  text        NOT NULL UNIQUE,
    "password"  text        NOT NULL,
    -- ROLE_* bits from the client's script/sys/service.py (ROLE_LOGIN|ROLE_PLAYER).
    "role"      bigint      NOT NULL DEFAULT 3,
    "created"   timestamptz NOT NULL DEFAULT now(),
    "lastLogin" timestamptz
);
