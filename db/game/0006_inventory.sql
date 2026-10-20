-- Passwords on secure containers and construction platforms
-- (inventory SetPassword / SetPassword2; List/Build(securityCode=...)).
CREATE TABLE game.container_passwords (
    "itemID"   bigint  NOT NULL REFERENCES game.items ON DELETE CASCADE,
    "which"    integer NOT NULL,
    "password" text    NOT NULL,
    PRIMARY KEY ("itemID", "which")
);
