//! `corpmgr`: corporations (ui/shared/neocom/corporation/*, station lobby,
//! infosvc). NPC corporations come from static.npc_corporations; players found
//! their own (game.corporations, IDs 140,000,000+).
//!
//! Membership is the character's corporationID plus its role bits
//! (const.corpRole*), mirrored in the session's corpid/corprole: the client
//! decides most UserIs*() itself from corprole and only asks the server for
//! corporations other than its own. Members are told OnCharacterJoined/Left-
//! Corporation(charID), OnCorporationCEOChanged(corpID, new, old),
//! OnCorporationLogoChanged(corpID), OnCorporationVoteIssued(corpID,
//! voteCaseID), OnCharacterCorporationDataChanged(), and applicants and
//! personnel managers OnCorporationApplicationChanged(status, corpID, charID).

use marshal::Value;
use proto::gpcs::Call;

use super::{user_error, Args, CallError, CallResult, Session};
use crate::{db, rows, Server};

/// TEMPLATE DEFAULT: what founding a corporation costs.
pub const CORPORATION_COST: f64 = 1_599_800.0;
/// TEMPLATE DEFAULT: shares a new corporation issues to its founder.
const FOUNDING_SHARES: i32 = 1000;
/// TEMPLATE DEFAULT: an office's rent per rental period (const.rentalPeriodOffice days).
pub const OFFICE_RENT: f64 = 10_000.0;

pub fn is_player_corp(id: i32) -> bool {
    (140_000_000..150_000_000).contains(&id)
}

pub const CORP_HEADER: [&str; 23] = [
    "corporationID",
    "corporationName",
    "tickerName",
    "description",
    "url",
    "taxRate",
    "ceoID",
    "creatorID",
    "stationID",
    "shares",
    "memberCount",
    "memberLimit",
    "allowedMemberRaceIDs",
    "shape1",
    "shape2",
    "shape3",
    "color1",
    "color2",
    "color3",
    "hasPlayerPersonnelManager",
    "factionID",
    "allianceID",
    "deleted",
];

const CORP_SELECT: &str = r#"
    SELECT c."corporationID", c."corporationName", c."tickerName", c."description", c."url", c."taxRate",
           c."ceoID", c."creatorID", c."stationID", c."shares",
           (SELECT count(*)::int FROM game.characters ch WHERE ch."corporationID" = c."corporationID" AND ch."deleted" IS NULL),
           c."memberLimit", c."allowedMemberRaceIDs", c."shape1", c."shape2", c."shape3", c."color1", c."color2", c."color3",
           1, NULL::integer, NULL::integer, c."deleted"
    FROM game.corporations c WHERE c."deleted" = 0 AND ($1::integer IS NULL OR c."corporationID" = $1)
    UNION ALL
    SELECT n."corporationID", n."corporationName", n."tickerName", n."description", '', 0.0::float8,
           1, 1, n."stationID", 0,
           (SELECT count(*)::int FROM game.characters ch WHERE ch."corporationID" = n."corporationID" AND ch."deleted" IS NULL),
           0, 15, NULL::integer, NULL::integer, NULL::integer, NULL::integer, NULL::integer, NULL::integer,
           0, n."factionID", NULL::integer, 0
    FROM static.npc_corporations n WHERE ($1::integer IS NULL OR n."corporationID" = $1)"#;

/// One corporation as util.Row (infosvc, GetCorporations, GetPublicInfo).
pub async fn corp_row(server: &Server, corp: i32) -> anyhow::Result<Value> {
    let c = server.pool.get().await?;
    let r = c.query_opt(CORP_SELECT, &[&Some(corp)]).await?;
    Ok(match r {
        Some(r) => rows::row(&CORP_HEADER, db::row_values(&r)?),
        None => Value::None,
    })
}

async fn all_corps(server: &Server) -> anyhow::Result<Value> {
    let c = server.pool.get().await?;
    let rs = c.query(CORP_SELECT, &[&None::<i32>]).await?;
    Ok(rows::rowset(
        &CORP_HEADER,
        rs.iter()
            .map(db::row_values)
            .collect::<anyhow::Result<_>>()?,
    ))
}

struct Role {
    name: &'static str,
    short: &'static str,
    description: &'static str,
}

/// TEMPLATE DEFAULT texts; the IDs are const.corpRole*, the names
/// 'role' + the constant's suffix (members.py looks for 'roleDirector').
const ROLES: [Role; 5] = [
    Role {
        name: "Director",
        short: "Director",
        description: "Can do anything the CEO can, except resign as CEO.",
    },
    Role {
        name: "PersonnelManager",
        short: "Personnel Manager",
        description: "Can accept applications and manage member roles.",
    },
    Role {
        name: "Accountant",
        short: "Accountant",
        description: "Can view and use the corporation's accounts.",
    },
    Role {
        name: "FactoryManager",
        short: "Factory Manager",
        description: "Can manage the corporation's factory slots.",
    },
    Role {
        name: "StationManager",
        short: "Station Manager",
        description: "Can manage the corporation's stations.",
    },
];

fn role_id(server: &Server, name: &str) -> i64 {
    server.world.constant(&format!("corpRole{name}")) as i64
}

fn all_roles(server: &Server) -> i64 {
    ROLES
        .iter()
        .map(|r| role_id(server, r.name))
        .fold(0, |a, b| a | b)
}

/// (corporationID, roles, grantableRoles) of a character.
async fn membership(server: &Server, char_id: i32) -> anyhow::Result<Option<(i32, i64, i64)>> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(
        r#"SELECT "corporationID", "corporationRoles", "grantableRoles" FROM game.characters WHERE "characterID" = $1"#,
        &[&char_id],
    )
    .await?
    .map(|r| (r.get(0), r.get(1), r.get(2))))
}

async fn ceo_of(server: &Server, corp: i32) -> anyhow::Result<Option<i32>> {
    if !is_player_corp(corp) {
        return Ok(None);
    }
    Ok(db::scalar::<Option<i32>>(
        &server.pool,
        r#"SELECT "ceoID" FROM game.corporations WHERE "corporationID" = $1"#,
        &[&corp],
    )
    .await?
    .flatten())
}

/// Does the caller hold any of `roles` in `corp` (directors and the CEO hold all)?
pub async fn has_role(
    server: &Server,
    me: i32,
    corp: i32,
    role_names: &[&str],
) -> anyhow::Result<bool> {
    let Some((my_corp, roles, _)) = membership(server, me).await? else {
        return Ok(false);
    };
    if my_corp != corp {
        return Ok(false);
    }
    if ceo_of(server, corp).await? == Some(me) {
        return Ok(true);
    }
    let mut mask = role_id(server, "Director");
    for r in role_names {
        mask |= role_id(server, r);
    }
    Ok(roles & mask != 0)
}

pub async fn is_accountant(server: &Server, session: &Session, corp: i32) -> anyhow::Result<bool> {
    has_role(
        server,
        session.char_id().unwrap_or(0),
        corp,
        &["Accountant"],
    )
    .await
}

async fn member_ids(server: &Server, corp: i32) -> anyhow::Result<Vec<i32>> {
    let c = server.pool.get().await?;
    Ok(c.query(r#"SELECT "characterID" FROM game.characters WHERE "corporationID" = $1 AND "deleted" IS NULL"#, &[&corp])
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

async fn tell_members(
    server: &Server,
    corp: i32,
    name: &str,
    args: Vec<Value>,
) -> anyhow::Result<()> {
    let members = member_ids(server, corp).await?;
    super::notify_chars(server, &members, name, args);
    Ok(())
}

/// The NPC corporation a character falls back to: its school's.
async fn school_corp(server: &Server, char_id: i32) -> anyhow::Result<i32> {
    let c = server.pool.get().await?;
    Ok(c.query_opt(
        r#"SELECT s."corporationID" FROM game.characters ch JOIN static.cc_schools s USING ("schoolID")
           WHERE ch."characterID" = $1"#,
        &[&char_id],
    )
    .await?
    .map(|r| r.get(0))
    .unwrap_or(1000167))
}

/// Moves a character into `corp` with the given roles.
pub async fn join(
    server: &Server,
    session: &mut Session,
    char_id: i32,
    corp: i32,
    roles: i64,
    grantable: i64,
) -> anyhow::Result<()> {
    let old = membership(server, char_id).await?.map(|m| m.0);
    db::exec(
        &server.pool,
        r#"UPDATE game.characters SET "corporationID" = $2, "corporationRoles" = $3, "grantableRoles" = $4,
               "title" = '', "startDateTime" = now() WHERE "characterID" = $1"#,
        &[&char_id, &corp, &roles, &grantable],
    )
    .await?;
    db::exec(
        &server.pool,
        r#"DELETE FROM game.corp_applications WHERE "characterID" = $1"#,
        &[&char_id],
    )
    .await?;
    super::change_char_session(
        server,
        session,
        char_id,
        vec![
            ("corpid", Value::Int(corp)),
            ("corprole", Value::from(roles)),
        ],
    );
    if let Some(old) = old.filter(|&o| o != corp) {
        tell_members(
            server,
            old,
            "OnCharacterLeftCorporation",
            vec![Value::Int(char_id)],
        )
        .await?;
    }
    tell_members(
        server,
        corp,
        "OnCharacterJoinedCorporation",
        vec![Value::Int(char_id)],
    )
    .await?;
    Ok(())
}

async fn set_roles(
    server: &Server,
    session: &mut Session,
    char_id: i32,
    roles: i64,
    grantable: i64,
) -> anyhow::Result<()> {
    db::exec(
        &server.pool,
        r#"UPDATE game.characters SET "corporationRoles" = $2, "grantableRoles" = $3 WHERE "characterID" = $1"#,
        &[&char_id, &roles, &grantable],
    )
    .await?;
    super::change_char_session(
        server,
        session,
        char_id,
        vec![("corprole", Value::from(roles))],
    );
    super::notify_chars(
        server,
        &[char_id],
        "OnCharacterCorporationDataChanged",
        vec![],
    );
    Ok(())
}

pub async fn move_shares(
    server: &Server,
    session: &mut Session,
    to: i32,
    amount: i32,
) -> Result<(), CallError> {
    let me = session.char_id().unwrap_or(0);
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    if amount <= 0 {
        return Ok(());
    }
    if !has_role(server, me, corp, &[]).await? {
        return Err(user_error("CrpAccessDenied", vec![]));
    }
    let mut c = server.pool.get().await?;
    let tx = c.transaction().await?;
    let held: Option<i32> = tx
        .query_opt(
            r#"SELECT "shares" FROM game.shares WHERE "ownerID" = $1 AND "corporationID" = $1"#,
            &[&corp],
        )
        .await?
        .map(|r| r.get(0));
    if held.unwrap_or(0) < amount {
        return Err(user_error("CrpNotEnoughShares", vec![]));
    }
    tx.execute(r#"UPDATE game.shares SET "shares" = "shares" - $2 WHERE "ownerID" = $1 AND "corporationID" = $1"#, &[&corp, &amount])
        .await?;
    tx.execute(
        r#"INSERT INTO game.shares ("ownerID", "corporationID", "shares") VALUES ($1, $2, $3)
           ON CONFLICT ("ownerID", "corporationID") DO UPDATE SET "shares" = game.shares."shares" + $3"#,
        &[&to, &corp, &amount],
    )
    .await?;
    tx.commit().await?;
    super::notify_chars(
        server,
        &[to],
        "OnShareChange",
        vec![Value::Int(to), Value::Int(corp)],
    );
    tell_members(
        server,
        corp,
        "OnShareChange",
        vec![Value::Int(corp), Value::Int(corp)],
    )
    .await?;
    Ok(())
}

async fn members(server: &Server, corp: i32, char_id: Option<i32>) -> anyhow::Result<Value> {
    db::rowset(
        &server.pool,
        r#"SELECT ch."characterID", ch."characterName", ch."corporationID", ch."title",
                  ch."corporationRoles" AS "roles", ch."grantableRoles", ch."startDateTime",
                  coalesce(s."shares", 0) AS "shares"
           FROM game.characters ch LEFT JOIN game.shares s ON s."ownerID" = ch."characterID" AND s."corporationID" = $1
           WHERE ch."corporationID" = $1 AND ch."deleted" IS NULL AND ($2::integer IS NULL OR ch."characterID" = $2)
           ORDER BY ch."characterName""#,
        &[&corp, &char_id],
    )
    .await
}

/// Closes ended votes, recording each winner as a sanctioned action.
async fn close_votes(server: &Server, corp: i32) -> anyhow::Result<()> {
    let c = server.pool.get().await?;
    let ended = c
        .query(
            r#"UPDATE game.vote_cases SET "closed" = 1 WHERE "corporationID" = $1 AND "closed" = 0 AND "endDateTime" <= now()
               RETURNING "voteCaseID", "voteType""#,
            &[&corp],
        )
        .await?;
    for r in ended {
        let case: i32 = r.get(0);
        let vote_type: i32 = r.get(1);
        let winner = c
            .query_opt(
                r#"SELECT o."optionID", o."parameter" FROM game.vote_options o
                   LEFT JOIN game.votes v ON v."voteCaseID" = o."voteCaseID" AND v."optionID" = o."optionID"
                   WHERE o."voteCaseID" = $1 GROUP BY o."optionID", o."parameter"
                   ORDER BY coalesce(sum(v."weight"), 0) DESC, o."optionID" LIMIT 1"#,
                &[&case],
            )
            .await?;
        if let Some(w) = winner {
            c.execute(
                r#"INSERT INTO game.sanctioned_actions ("voteCaseID", "corporationID", "voteType", "optionID", "parameter", "expires")
                   VALUES ($1, $2, $3, $4, $5, now() + interval '7 days') ON CONFLICT DO NOTHING"#,
                &[&case, &corp, &vote_type, &w.get::<_, i32>(0), &w.get::<_, i32>(1)],
            )
            .await?;
        }
    }
    Ok(())
}

/// Applies a sanctioned action (UpdateSanctionedAction).
async fn enact(
    server: &Server,
    session: &mut Session,
    corp: i32,
    vote_type: i32,
    parameter: i32,
) -> anyhow::Result<()> {
    let w = &server.world;
    if parameter == 0 && vote_type != w.constant("voteGeneral") {
        return Ok(());
    }
    if vote_type == w.constant("voteCEO") {
        let old = ceo_of(server, corp).await?;
        db::exec(
            &server.pool,
            r#"UPDATE game.corporations SET "ceoID" = $2 WHERE "corporationID" = $1"#,
            &[&corp, &parameter],
        )
        .await?;
        let all = all_roles(server);
        set_roles(server, session, parameter, all, all).await?;
        tell_members(
            server,
            corp,
            "OnCorporationCEOChanged",
            vec![Value::Int(corp), Value::Int(parameter), Value::from(old)],
        )
        .await?;
    } else if vote_type == w.constant("voteWar") {
        declare_war(server, corp, parameter).await?;
    } else if vote_type == w.constant("voteShares") {
        db::exec(
            &server.pool,
            r#"UPDATE game.corporations SET "shares" = "shares" + $2 WHERE "corporationID" = $1"#,
            &[&corp, &parameter],
        )
        .await?;
        db::exec(
            &server.pool,
            r#"INSERT INTO game.shares ("ownerID", "corporationID", "shares") VALUES ($1, $1, $2)
               ON CONFLICT ("ownerID", "corporationID") DO UPDATE SET "shares" = game.shares."shares" + $2"#,
            &[&corp, &parameter],
        )
        .await?;
    } else if vote_type == w.constant("voteKickMember") {
        let fallback = school_corp(server, parameter).await?;
        join(server, session, parameter, fallback, 0, 0).await?;
    }
    Ok(())
}

async fn declare_war(server: &Server, by: i32, against: i32) -> anyhow::Result<()> {
    let c = server.pool.get().await?;
    let at: std::time::SystemTime = c
        .query_one(
            r#"INSERT INTO game.wars ("declaredBy", "against") VALUES ($1, $2) RETURNING "timeDeclared""#,
            &[&by, &against],
        )
        .await?
        .get(0);
    for corp in [by, against] {
        tell_members(
            server,
            corp,
            "OnWarDeclaration",
            vec![
                Value::Int(by),
                Value::Int(against),
                Value::Int(1),
                Value::Long(db::blue_time(at)),
            ],
        )
        .await?;
    }
    Ok(())
}

fn ticker_ok(t: &str) -> bool {
    (2..=5).contains(&t.len())
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

/// util.Rowset sent by the client (votes.py options): its lines.
fn rowset_lines(v: Option<&Value>) -> Vec<Vec<Value>> {
    let state = match v {
        Some(Value::Instance { state, .. }) => state.as_ref(),
        _ => return Vec::new(),
    };
    state
        .get("lines")
        .and_then(Value::as_seq)
        .map(|ls| {
            ls.iter()
                .filter_map(|l| l.as_seq().map(<[Value]>::to_vec))
                .collect()
        })
        .unwrap_or_default()
}

/// The parts of corp.GetStartupData the server owns.
pub async fn logon_data(server: &Server, session: &Session) -> CallResult {
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    let mut fields = vec![
        ("GetPublicInfo", corp_row(server, corp).await?),
        (
            "GetMembersIDs",
            Value::List(
                member_ids(server, corp)
                    .await?
                    .into_iter()
                    .map(Value::Int)
                    .collect(),
            ),
        ),
    ];
    if let Some(st) = session
        .attrs
        .int("stationid")
        .and_then(|s| server.world.stations.get(&(s as i32)))
    {
        fields.push((
            "GetPublicStationInfo",
            super::station::public_station_info(server, st),
        ));
        fields.push((
            "GetCorporationsAtStation",
            corporations_at_station(server, st.station_id).await?,
        ));
        fields.push(("GetOffices", offices(server, st.station_id).await?));
        fields.push(("GetStationItemBits", super::station::station_item_bits(st)));
    }
    Ok(rows::keyval(fields))
}

/// Corporations with an office here, plus the owner (lobby list; base_corporation
/// primes corporationID, ceoID, creatorID).
pub async fn corporations_at_station(server: &Server, station: i32) -> anyhow::Result<Value> {
    db::rowset(
        &server.pool,
        r#"SELECT c."corporationID", c."corporationName", c."ceoID", c."creatorID"
           FROM game.offices o JOIN game.corporations c USING ("corporationID")
           WHERE o."stationID" = $1 ORDER BY c."corporationName""#,
        &[&station],
    )
    .await
}

/// Offices here: hangar.py/base_market.py open each.officeFolderID (the folder
/// item holding the corporation's hangar) and each.itemID.
pub async fn offices(server: &Server, station: i32) -> anyhow::Result<Value> {
    db::rowset(
        &server.pool,
        r#"SELECT c."corporationID", o."officeID", o."officeID" AS "folderID", o."officeID" AS "officeFolderID",
                  o."officeID" AS "itemID", c."ceoID", c."creatorID", o."stationID"
           FROM game.offices o JOIN game.corporations c USING ("corporationID") WHERE o."stationID" = $1"#,
        &[&station],
    )
    .await
}

pub async fn dispatch(
    server: &Server,
    session: &mut Session,
    method: &str,
    call: &Call,
) -> CallResult {
    let a = Args::of(call);
    let me = session.char_id().unwrap_or(0);
    let my_corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    let corp_arg = |i: usize| a.i32(i).unwrap_or(my_corp);
    let w = &server.world;
    match method {
        "GetLogonData" => logon_data(server, session).await,
        // GetCorporations(): every corporation; GetCorporations(id) or ([id]): one row.
        "GetCorporations" => match a.pos.first() {
            None | Some(Value::None) => Ok(all_corps(server).await?),
            Some(_) => Ok(corp_row(server, a.ints(0).first().copied().unwrap_or(0) as i32).await?),
        },
        "GetPublicInfo" => Ok(corp_row(server, corp_arg(0)).await?),
        "GetCorporationIDForCharacter" => Ok(Value::from(membership(server, a.i32(0).unwrap_or(0)).await?.map(|m| m.0))),
        "GetMembersIDs" => Ok(Value::List(member_ids(server, my_corp).await?.into_iter().map(Value::Int).collect())),
        "GetMembers" => Ok(members(server, corp_arg(0), a.i32(1)).await?),
        // votes.py: shareholders worth listing (title, characterName, characterID).
        "GetMembersWithNamesWithMoreThanAvgShares" => Ok(members(server, corp_arg(0), None).await?),
        "GetRoles" => Ok(rows::rowset(
            &["roleID", "roleName", "shortDescription", "description"],
            ROLES
                .iter()
                .map(|r| {
                    vec![Value::from(role_id(server, r.name)), Value::from(format!("role{}", r.name)), Value::str(r.short), Value::str(r.description)]
                })
                .collect(),
        )),
        "GetGrantableRolesForCharacter" => {
            let corp = corp_arg(0);
            if ceo_of(server, corp).await? == Some(me) {
                return Ok(Value::from(all_roles(server)));
            }
            Ok(Value::from(membership(server, me).await?.filter(|m| m.0 == corp).map(|m| m.2).unwrap_or(0)))
        }
        "UserIsCEO" | "UserIsActiveCEO" => Ok(Value::from(ceo_of(server, corp_arg(0)).await? == Some(me))),
        "UserIsActiveCEOorEquivalent" => Ok(Value::from(has_role(server, me, corp_arg(0), &[]).await?)),
        "UserIsPersonnelManagerOrEquivalent" => Ok(Value::from(has_role(server, me, corp_arg(0), &["PersonnelManager"]).await?)),
        "UserIsAccountantOrEquivalent" => Ok(Value::from(has_role(server, me, corp_arg(0), &["Accountant"]).await?)),
        "UserIsFactoryManager" => Ok(Value::from(has_role(server, me, corp_arg(0), &["FactoryManager"]).await?)),
        "UserIsStationManager" => Ok(Value::from(has_role(server, me, corp_arg(0), &["StationManager"]).await?)),
        // Anyone not leading a player corporation may found one.
        "UserCanCreateCorporation" => {
            let leads = db::scalar::<i32>(
                &server.pool,
                r#"SELECT 1 FROM game.corporations WHERE "ceoID" = $1 AND "deleted" = 0"#,
                &[&me],
            )
            .await?
            .is_some();
            Ok(Value::from(!leads))
        }
        // Members of a player corporation may stand for CEO.
        "CharCanRunForCEO" => {
            let corp = corp_arg(0);
            Ok(Value::from(is_player_corp(corp) && my_corp == corp && ceo_of(server, corp).await? != Some(me)))
        }
        "CanViewVotes" => {
            let corp = corp_arg(0);
            let holds = db::scalar::<i32>(
                &server.pool,
                r#"SELECT "shares" FROM game.shares WHERE "ownerID" = $1 AND "corporationID" = $2 AND "shares" > 0"#,
                &[&me, &corp],
            )
            .await?
            .is_some();
            Ok(Value::from(is_player_corp(corp) && (holds || my_corp == corp)))
        }
        "CanBeKickedOut" => {
            let corp = corp_arg(0);
            let who = a.i32(1).unwrap_or(0);
            let ok = is_player_corp(corp)
                && who != me
                && ceo_of(server, corp).await? != Some(who)
                && has_role(server, me, corp, &["PersonnelManager"]).await?;
            Ok(Value::from(ok))
        }
        "GetCostForCreatingACorporation" => Ok(Value::Float(CORPORATION_COST)),
        "GetSuggestedTickerNames" => {
            let name = a.text(0).unwrap_or_default();
            let words: Vec<String> = name.split_whitespace().map(|w| w.to_uppercase()).collect();
            let initials: String = words.iter().filter_map(|w| w.chars().next()).filter(char::is_ascii_alphanumeric).collect();
            let compact: String = name.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_uppercase();
            let mut out: Vec<String> = Vec::new();
            for cand in [initials.clone(), compact.chars().take(3).collect(), compact.chars().take(4).collect(), compact.chars().take(5).collect()] {
                if ticker_ok(&cand) && !out.contains(&cand) {
                    let taken = db::scalar::<i32>(
                        &server.pool,
                        r#"SELECT 1 FROM game.corporations WHERE upper("tickerName") = $1 AND "deleted" = 0"#,
                        &[&cand],
                    )
                    .await?
                    .is_some();
                    if !taken {
                        out.push(cand);
                    }
                }
            }
            Ok(rows::rowset(&["tickerName"], out.into_iter().map(|t| vec![Value::from(t)]).collect()))
        }
        // AddCorporation(name, ticker, description, url, taxRate, shape1..3, color1..3)
        "AddCorporation" => {
            let Some(station) = session.attrs.int("stationid") else {
                return Err(user_error("CanOnlyCreateCorpInStation", vec![]));
            };
            let name = a.text(0).unwrap_or_default().trim().to_owned();
            let ticker = a.text(1).unwrap_or_default().trim().to_uppercase();
            if name.len() < 4 || name.len() > 24 {
                return Err(user_error("CorpNameInvalid", vec![]));
            }
            if !ticker_ok(&ticker) {
                return Err(user_error("CorpTickerNameInvalid", vec![]));
            }
            if ceo_of(server, my_corp).await? == Some(me) {
                return Err(user_error("CEOCannotCreateCorporation", vec![]));
            }
            let taken = db::scalar::<i32>(
                &server.pool,
                r#"SELECT 1 FROM game.corporations WHERE (lower("corporationName") = lower($1) OR upper("tickerName") = $2) AND "deleted" = 0
                   UNION ALL SELECT 1 FROM static.npc_corporations WHERE lower("corporationName") = lower($1)"#,
                &[&name, &ticker],
            )
            .await?
            .is_some();
            if taken {
                return Err(user_error("CorpNameInvalidTaken", vec![]));
            }
            let station_owner = w.stations.get(&(station as i32)).map(|s| s.corporation_id).unwrap_or(1);
            super::wallet::transfer(server, me, station_owner, CORPORATION_COST, "refCorporationPayment", None, "Corporation registration").await?;
            let shapes: Vec<Option<i32>> = (5..11).map(|i| a.i32(i)).collect();
            let c = server.pool.get().await?;
            let corp: i32 = c
                .query_one(
                    r#"INSERT INTO game.corporations ("corporationName", "tickerName", "description", "url", "taxRate", "ceoID",
                           "creatorID", "stationID", "shares", "shape1", "shape2", "shape3", "color1", "color2", "color3")
                       VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING "corporationID""#,
                    &[
                        &name,
                        &ticker,
                        &a.text(2).unwrap_or_default(),
                        &a.text(3).unwrap_or_default(),
                        &a.float(4).unwrap_or(0.0).clamp(0.0, 1.0),
                        &me,
                        &(station as i32),
                        &FOUNDING_SHARES,
                        &shapes[0],
                        &shapes[1],
                        &shapes[2],
                        &shapes[3],
                        &shapes[4],
                        &shapes[5],
                    ],
                )
                .await?
                .get(0);
            c.execute(
                r#"INSERT INTO game.shares ("ownerID", "corporationID", "shares") VALUES ($1, $2, $3)"#,
                &[&me, &corp, &FOUNDING_SHARES],
            )
            .await?;
            let all = all_roles(server);
            join(server, session, me, corp, all, all).await?;
            Ok(Value::Int(corp))
        }
        // UpdateCorporation(corpID, description, url, taxRate)
        "UpdateCorporation" => {
            let corp = corp_arg(0);
            if !has_role(server, me, corp, &[]).await? {
                return Err(user_error("OnlyCEOOrEquivCanEditCorp", vec![]));
            }
            db::exec(
                &server.pool,
                r#"UPDATE game.corporations SET "description" = $2, "url" = $3, "taxRate" = $4 WHERE "corporationID" = $1"#,
                &[&corp, &a.text(1).unwrap_or_default(), &a.text(2).unwrap_or_default(), &a.float(3).unwrap_or(0.0).clamp(0.0, 1.0)],
            )
            .await?;
            tell_members(server, corp, "OnCharacterCorporationDataChanged", vec![]).await?;
            Ok(Value::None)
        }
        // UpdateLogo(corpID, shape1..3, color1..3, _)
        "UpdateLogo" => {
            let corp = corp_arg(0);
            if !has_role(server, me, corp, &[]).await? {
                return Err(user_error("OnlyCEOOrEquivCanEditCorp", vec![]));
            }
            super::wallet::transfer(server, corp, 1, w.constant("corpLogoChangeCost") as f64, "refCorporationPayment", Some(me as i64), "Logo change")
                .await?;
            let v: Vec<Option<i32>> = (1..7).map(|i| a.i32(i)).collect();
            db::exec(
                &server.pool,
                r#"UPDATE game.corporations SET "shape1" = $2, "shape2" = $3, "shape3" = $4, "color1" = $5, "color2" = $6, "color3" = $7
                   WHERE "corporationID" = $1"#,
                &[&corp, &v[0], &v[1], &v[2], &v[3], &v[4], &v[5]],
            )
            .await?;
            tell_members(server, corp, "OnCorporationLogoChanged", vec![Value::Int(corp)]).await?;
            Ok(Value::None)
        }
        // Member limit from the CEO's skills: 10 per level of Corporation
        // Management (TEMPLATE DEFAULT rate), and every race allowed.
        "UpdateCorporationAbilities" => {
            if !is_player_corp(my_corp) || ceo_of(server, my_corp).await? != Some(me) {
                return Ok(Value::None);
            }
            let level = super::skills::skill_level(server, me, w.type_id("CorporationManagement")).await?;
            db::exec(
                &server.pool,
                r#"UPDATE game.corporations SET "memberLimit" = $2 WHERE "corporationID" = $1"#,
                &[&my_corp, &(10 + 10 * level)],
            )
            .await?;
            Ok(Value::Int(1))
        }
        "AddRoleToMember" | "RemoveRoleFromMember" | "AddGrantableRoleToMember" | "RemoveGrantableRoleFromMember" => {
            let who = a.i32(0).unwrap_or(0);
            let role = a.int(1).unwrap_or(0);
            let Some((corp, roles, grantable)) = membership(server, who).await? else { return Ok(Value::None) };
            let is_ceo = ceo_of(server, corp).await? == Some(me);
            let my_grantable = if is_ceo { all_roles(server) } else { membership(server, me).await?.filter(|m| m.0 == corp).map(|m| m.2).unwrap_or(0) };
            let directing = has_role(server, me, corp, &[]).await?;
            let grant_change = method.contains("Grantable");
            if (grant_change && !directing) || (!grant_change && my_grantable & role != role && !directing) {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let (mut roles, mut grantable) = (roles, grantable);
            match method {
                "AddRoleToMember" => roles |= role,
                "RemoveRoleFromMember" => roles &= !role,
                "AddGrantableRoleToMember" => grantable |= role,
                _ => grantable &= !role,
            }
            set_roles(server, session, who, roles, grantable).await?;
            Ok(Value::None)
        }
        // UpdateMember(charID, corpID, title)
        "UpdateMember" => {
            let corp = corp_arg(1);
            if !has_role(server, me, corp, &[]).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            db::exec(
                &server.pool,
                r#"UPDATE game.characters SET "title" = $3 WHERE "characterID" = $1 AND "corporationID" = $2"#,
                &[&a.i32(0).unwrap_or(0), &corp, &a.text(2).unwrap_or_default()],
            )
            .await?;
            Ok(Value::None)
        }
        // KickOutMember(corpID, charID): the member goes back to its school.
        "KickOutMember" => {
            let corp = corp_arg(0);
            let who = a.i32(1).unwrap_or(0);
            if !is_player_corp(corp) || ceo_of(server, corp).await? == Some(who) || !has_role(server, me, corp, &["PersonnelManager"]).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let fallback = school_corp(server, who).await?;
            join(server, session, who, fallback, 0, 0).await?;
            Ok(Value::Int(1))
        }
        // ResignFromCEO(): the candidates. ResignFromCEO(newCeoID): hand over.
        "ResignFromCEO" => {
            if ceo_of(server, my_corp).await? != Some(me) {
                return Err(user_error("OnlyCEOCanResign", vec![]));
            }
            match a.i32(0) {
                None => {
                    let others: Vec<Value> = member_ids(server, my_corp).await?.into_iter().filter(|&m| m != me).map(Value::Int).collect();
                    if others.is_empty() {
                        return Err(user_error("CrpCantResignNoOtherMembers", vec![]));
                    }
                    Ok(Value::List(others))
                }
                Some(new) => {
                    if membership(server, new).await?.map(|m| m.0) != Some(my_corp) {
                        return Ok(Value::None);
                    }
                    db::exec(&server.pool, r#"UPDATE game.corporations SET "ceoID" = $2 WHERE "corporationID" = $1"#, &[&my_corp, &new]).await?;
                    let all = all_roles(server);
                    set_roles(server, session, new, all, all).await?;
                    set_roles(server, session, me, role_id(server, "Director"), all).await?;
                    tell_members(server, my_corp, "OnCorporationCEOChanged", vec![Value::Int(my_corp), Value::Int(new), Value::Int(me)]).await?;
                    Ok(Value::None)
                }
            }
        }
        // ---------------------------------------------------- applications
        // InsertApplication(corpID, charID, text, roles, grantableRoles). NPC
        // corporations have no personnel manager: they take everyone.
        "InsertApplication" => {
            let corp = a.i32(0).unwrap_or(0);
            if corp == my_corp {
                return Err(user_error("CanNotJoinCorpAlreadyAMember", vec![]));
            }
            if ceo_of(server, my_corp).await? == Some(me) {
                return Err(user_error("CeoCanNotJoinACorp", vec![]));
            }
            if !is_player_corp(corp) {
                join(server, session, me, corp, 0, 0).await?;
                return Ok(Value::None);
            }
            let status = w.constant("crpApplicationAppliedByCharacter");
            db::exec(
                &server.pool,
                r#"INSERT INTO game.corp_applications ("corporationID", "characterID", "applicationText", "roles", "grantableRoles", "status")
                   VALUES ($1, $2, $3, $4, $5, $6)
                   ON CONFLICT ("corporationID", "characterID") DO UPDATE SET "applicationText" = $3, "roles" = $4,
                       "grantableRoles" = $5, "status" = $6, "applicationDateTime" = now()"#,
                &[&corp, &me, &a.text(2).unwrap_or_default(), &a.int(3).unwrap_or(0), &a.int(4).unwrap_or(0), &status],
            )
            .await?;
            application_changed(server, status, corp, me).await?;
            Ok(Value::None)
        }
        // GetApplications(-1, charID): mine; GetApplications(corpID[, charID]): the corp's.
        "GetApplications" => {
            let corp = a.i32(0).unwrap_or(-1);
            let who = a.i32(1);
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "corporationID", "characterID", "applicationText", "roles", "grantableRoles", "status", "applicationDateTime"
                   FROM game.corp_applications
                   WHERE ($1 = -1 OR "corporationID" = $1) AND ($2::integer IS NULL OR "characterID" = $2)
                   ORDER BY "applicationDateTime""#,
                &[&corp, &if corp == -1 { Some(who.unwrap_or(me)) } else { who }],
            )
            .await?)
        }
        // UpdateApplication(corpID, charID, text, roles, grantableRoles, status):
        // both sides accepting makes the applicant a member.
        "UpdateApplication" => {
            let corp = a.i32(0).unwrap_or(0);
            let who = a.i32(1).unwrap_or(0);
            let status = a.i32(5).unwrap_or(0);
            if who != me && !has_role(server, me, corp, &["PersonnelManager"]).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let previous: Option<i32> = db::scalar(
                &server.pool,
                r#"SELECT "status" FROM game.corp_applications WHERE "corporationID" = $1 AND "characterID" = $2"#,
                &[&corp, &who],
            )
            .await?;
            let roles = a.int(3).unwrap_or(0);
            let grantable = a.int(4).unwrap_or(0);
            db::exec(
                &server.pool,
                r#"UPDATE game.corp_applications SET "applicationText" = $3, "roles" = $4, "grantableRoles" = $5, "status" = $6
                   WHERE "corporationID" = $1 AND "characterID" = $2"#,
                &[&corp, &who, &a.text(2).unwrap_or_default(), &roles, &grantable, &status],
            )
            .await?;
            let accepted_by_corp = w.constant("crpApplicationAcceptedByCorporation");
            let accepted_by_char = w.constant("crpApplicationAcceptedByCharacter");
            if (status == accepted_by_char && previous == Some(accepted_by_corp))
                || (status == accepted_by_corp && previous == Some(accepted_by_char))
            {
                join(server, session, who, corp, roles, grantable).await?;
            }
            application_changed(server, status, corp, who).await?;
            Ok(Value::None)
        }
        "DeleteApplication" => {
            let corp = a.i32(0).unwrap_or(0);
            let who = a.i32(1).unwrap_or(me);
            if who != me && !has_role(server, me, corp, &["PersonnelManager"]).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            db::exec(&server.pool, r#"DELETE FROM game.corp_applications WHERE "corporationID" = $1 AND "characterID" = $2"#, &[&corp, &who])
                .await?;
            application_changed(server, w.constant("crpApplicationRejectedByCharacter"), corp, who).await?;
            Ok(Value::None)
        }
        // ------------------------------------------------------------ votes
        // InsertVoteCase(text, description, corpID, voteType, options, start, end)
        "InsertVoteCase" => {
            let corp = corp_arg(2);
            if my_corp != corp {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let start = a.int(5).map(db::system_time).unwrap_or_else(std::time::SystemTime::now);
            let end = a.int(6).map(db::system_time).unwrap_or_else(|| std::time::SystemTime::now() + std::time::Duration::from_secs(86400));
            let c = server.pool.get().await?;
            let case: i32 = c
                .query_one(
                    r#"INSERT INTO game.vote_cases ("corporationID", "characterID", "voteType", "voteCaseText", "description",
                           "startDateTime", "endDateTime")
                       VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING "voteCaseID""#,
                    &[&corp, &me, &a.i32(3).unwrap_or(0), &a.text(0).unwrap_or_default(), &a.text(1).unwrap_or_default(), &start, &end],
                )
                .await?
                .get(0);
            for (i, line) in rowset_lines(a.pos.get(4)).into_iter().enumerate() {
                let text = line.first().and_then(Value::as_text).unwrap_or_default();
                let param = line.get(1).and_then(Value::as_int).unwrap_or(0) as i32;
                c.execute(
                    r#"INSERT INTO game.vote_options ("voteCaseID", "optionID", "optionText", "parameter") VALUES ($1, $2, $3, $4)"#,
                    &[&case, &(i as i32), &text, &param],
                )
                .await?;
            }
            tell_members(server, corp, "OnCorporationVoteIssued", vec![Value::Int(corp), Value::Int(case)]).await?;
            Ok(Value::Int(case))
        }
        // GetVoteCasesByCorporation(corpID, status): 2 open, 1 closed, else all.
        "GetVoteCasesByCorporation" => {
            let corp = corp_arg(0);
            close_votes(server, corp).await?;
            let status = a.int(1).unwrap_or(0) as i32;
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "voteCaseID", "corporationID", "characterID", "voteType", "voteCaseText", "description",
                          "startDateTime", "endDateTime"
                   FROM game.vote_cases WHERE "corporationID" = $1
                     AND ($2 NOT IN (1, 2) OR ($2 = 2 AND "closed" = 0) OR ($2 = 1 AND "closed" = 1))
                   ORDER BY "voteCaseID" DESC"#,
                &[&corp, &status],
            )
            .await?)
        }
        // votesFor is the option's share of the votes cast (votes.py draws it as a bar fraction).
        "GetVoteCaseOptions" => Ok(db::rowset(
            &server.pool,
            r#"SELECT o."voteCaseID", o."optionID", o."optionText", o."parameter",
                      coalesce(sum(v."weight"), 0)::float8 / greatest(1, (SELECT sum("weight") FROM game.votes WHERE "voteCaseID" = $1))
                          AS "votesFor"
               FROM game.vote_options o LEFT JOIN game.votes v ON v."voteCaseID" = o."voteCaseID" AND v."optionID" = o."optionID"
               WHERE o."voteCaseID" = $1 GROUP BY o."voteCaseID", o."optionID" ORDER BY o."optionID""#,
            &[&a.i32(0).unwrap_or(0)],
        )
        .await?),
        "GetVotes" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "voteCaseID", "characterID", "optionID" FROM game.votes WHERE "voteCaseID" = $1 AND "characterID" = $2"#,
            &[&a.i32(0).unwrap_or(0), &me],
        )
        .await?),
        // InsertVote(voteCaseID, optionID): weighted by shares held (at least one).
        "InsertVote" => {
            let case = a.i32(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let open: Option<i32> = c
                .query_opt(
                    r#"SELECT "corporationID" FROM game.vote_cases WHERE "voteCaseID" = $1 AND "closed" = 0 AND "endDateTime" > now()"#,
                    &[&case],
                )
                .await?
                .map(|r| r.get(0));
            let Some(corp) = open else { return Err(user_error("CrpVoteClosed", vec![])) };
            let weight: i32 = c
                .query_opt(r#"SELECT "shares" FROM game.shares WHERE "ownerID" = $1 AND "corporationID" = $2"#, &[&me, &corp])
                .await?
                .map(|r| r.get(0))
                .unwrap_or(0)
                .max(1);
            c.execute(
                r#"INSERT INTO game.votes ("voteCaseID", "characterID", "optionID", "weight") VALUES ($1, $2, $3, $4)
                   ON CONFLICT ("voteCaseID", "characterID") DO NOTHING"#,
                &[&case, &me, &a.i32(1).unwrap_or(0), &weight],
            )
            .await?;
            Ok(Value::None)
        }
        // GetSanctionedActionsByCorporation(corpID, inEffect)
        "GetSanctionedActionsByCorporation" => {
            let corp = corp_arg(0);
            close_votes(server, corp).await?;
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "voteCaseID", "corporationID", "voteType", "optionID", "parameter", "inEffect", "actedUpon",
                          "expires", "timeActedUpon", "timeRescended"
                   FROM game.sanctioned_actions WHERE "corporationID" = $1 AND "inEffect" = $2 ORDER BY "voteCaseID" DESC"#,
                &[&corp, &(a.int(1).unwrap_or(0) as i32)],
            )
            .await?)
        }
        // UpdateSanctionedAction(voteCaseID, inEffect): the CEO puts it into effect.
        "UpdateSanctionedAction" => {
            if ceo_of(server, my_corp).await? != Some(me) {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let case = a.i32(0).unwrap_or(0);
            let c = server.pool.get().await?;
            let Some(r) = c
                .query_opt(
                    r#"UPDATE game.sanctioned_actions SET "inEffect" = 1, "actedUpon" = 1, "timeActedUpon" = now()
                       WHERE "voteCaseID" = $1 AND "corporationID" = $2 AND "actedUpon" = 0 AND "expires" > now()
                       RETURNING "voteType", "parameter""#,
                    &[&case, &my_corp],
                )
                .await?
            else {
                return Ok(Value::None);
            };
            drop(c);
            enact(server, session, my_corp, r.get(0), r.get(1)).await?;
            Ok(Value::None)
        }
        // ------------------------------------------------------- wars etc.
        "GetWars" => {
            let corp = a.i32(0);
            Ok(db::rowset(
                &server.pool,
                r#"SELECT "warID", "declaredBy", "against", "timeDeclared", "timeFinished", "retracted", "canBlowStuffUp"
                   FROM game.wars WHERE $1::integer IS NULL OR "declaredBy" = $1 OR "against" = $1 ORDER BY "warID" DESC"#,
                &[&corp],
            )
            .await?)
        }
        "GetCorporationStandings" => Ok(db::rowset(
            &server.pool,
            r#"SELECT "fromID", "toID", "standing", "notes" FROM game.standings WHERE "fromID" = $1 ORDER BY "toID""#,
            &[&corp_arg(0)],
        )
        .await?),
        // UpdateCorporationStandings(toID, standing, notes)
        "UpdateCorporationStandings" => {
            if !has_role(server, me, my_corp, &[]).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let standing = a.float(1).unwrap_or(0.0).clamp(-10.0, 10.0);
            let notes = a.text(2).unwrap_or_default();
            super::standing::set_standing(server, my_corp, a.i32(0).unwrap_or(0), Some(standing), Some(&notes)).await?;
            Ok(Value::None)
        }
        // PayoutDividend(payShareholders, amount): per share to shareholders,
        // or evenly to members.
        "PayoutDividend" => {
            if ceo_of(server, my_corp).await? != Some(me) {
                return Err(user_error("OnlyCEOCanPayoutDividends", vec![]));
            }
            let amount = a.float(1).unwrap_or(0.0);
            let to_shareholders = a.int(0).unwrap_or(0) != 0;
            let c = server.pool.get().await?;
            let recipients: Vec<(i32, f64)> = if to_shareholders {
                c.query(
                    r#"SELECT "ownerID", "shares"::float8 FROM game.shares WHERE "corporationID" = $1 AND "shares" > 0 AND "ownerID" <> $1"#,
                    &[&my_corp],
                )
                .await?
                .iter()
                .map(|r| (r.get(0), r.get(1)))
                .collect()
            } else {
                member_ids(server, my_corp).await?.into_iter().map(|m| (m, 1.0)).collect()
            };
            drop(c);
            let total: f64 = recipients.iter().map(|r| r.1).sum();
            for (owner, weight) in recipients {
                if total > 0.0 {
                    super::wallet::transfer(server, my_corp, owner, amount * weight / total, "refCorporationPayment", Some(me as i64), "Dividend")
                        .await?;
                }
            }
            Ok(Value::None)
        }
        // corpmgr.GetCorporationWallet(corpID).List(): the corporation's wallet container.
        "GetCorporationWallet" => {
            let params = Value::tuple([
                Value::str("special"),
                Value::Int(w.constant("containerWallet")),
                Value::Int(corp_arg(0)),
            ]);
            Ok(session.bind_value(server, "inventory", params))
        }
        // accounts.py: inventory[flag][locationID].items -> description.item
        "GetCorporationInventory" => {
            let corp = corp_arg(0);
            let items = super::inventory::items_where(server, r#""ownerID" = $1"#, &[&corp]).await?;
            let mut by_flag: Vec<(i32, Vec<(i64, Vec<Value>)>)> = [w.flag("Wallet"), w.flag("Hangar"), w.flag("None")]
                .into_iter()
                .map(|f| (f, Vec::new()))
                .collect();
            for it in &items {
                let Some(slot) = by_flag.iter_mut().find(|(f, _)| *f == it.flag) else { continue };
                let desc = rows::keyval(vec![("item", it.row(server))]);
                match slot.1.iter_mut().find(|(loc, _)| *loc == it.location_id) {
                    Some(l) => l.1.push(desc),
                    None => slot.1.push((it.location_id, vec![desc])),
                }
            }
            Ok(Value::Dict(
                by_flag
                    .into_iter()
                    .map(|(flag, locs)| {
                        (
                            Value::Int(flag),
                            Value::Dict(
                                locs.into_iter().map(|(loc, items)| (Value::from(loc), rows::keyval(vec![("items", Value::List(items))]))).collect(),
                            ),
                        )
                    })
                    .collect(),
            ))
        }
        // pvptradewnd: stations the corporation is at (itemID, locationID, typeID).
        "GetCorporationStations" => {
            let c = server.pool.get().await?;
            let ids: Vec<i32> = c
                .query(r#"SELECT DISTINCT "stationID" FROM game.offices WHERE "corporationID" = $1"#, &[&my_corp])
                .await?
                .iter()
                .map(|r| r.get(0))
                .collect();
            let lines = ids
                .into_iter()
                .filter_map(|id| w.stations.get(&id))
                .map(|s| vec![Value::Int(s.station_id), Value::Int(s.solar_system_id), Value::Int(s.type_id)])
                .collect();
            Ok(rows::rowset(&["itemID", "locationID", "typeID"], lines))
        }
        // accounts.py CorpRents: offices rented by my corporation.
        "GetItemsRented" => Ok(db::rowset(
            &server.pool,
            &format!(
                r#"SELECT o."officeID" AS "itemID", {office}::int AS "typeID", 0 AS "flag", o."stationID" AS "rentedFromID",
                          o."rentedAt" AS "startDate", {days} AS "rentPeriodInDays", {rent}::float8 AS "periodCost",
                          o."rentedAt" + interval '{days} days' AS "balanceDueDate"
                   FROM game.offices o WHERE o."corporationID" = $1"#,
                office = w.type_id("OfficeFolder"),
                days = w.constant("rentalPeriodOffice"),
                rent = OFFICE_RENT,
            ),
            &[&my_corp],
        )
        .await?),
        m => super::unimplemented("corpmgr", m, call),
    }
}

async fn application_changed(
    server: &Server,
    status: i32,
    corp: i32,
    char_id: i32,
) -> anyhow::Result<()> {
    let mut told = vec![char_id];
    let c = server.pool.get().await?;
    let managers = c
        .query(
            r#"SELECT ch."characterID" FROM game.characters ch LEFT JOIN game.corporations c USING ("corporationID")
               WHERE ch."corporationID" = $1 AND (ch."corporationRoles" & $2 <> 0 OR c."ceoID" = ch."characterID")"#,
            &[&corp, &(server.world.constant("corpRoleDirector") as i64 | server.world.constant("corpRolePersonnelManager") as i64)],
        )
        .await?;
    told.extend(managers.iter().map(|r| r.get::<_, i32>(0)));
    super::notify_chars(
        server,
        &told,
        "OnCorporationApplicationChanged",
        vec![Value::Int(status), Value::Int(corp), Value::Int(char_id)],
    );
    Ok(())
}

/// corpStationMgr office calls (station lobby).
pub async fn station_mgr(server: &Server, session: &mut Session, call: &Call) -> CallResult {
    let me = session.char_id().unwrap_or(0);
    let corp = session.attrs.int("corpid").unwrap_or(0) as i32;
    let station = session.attrs.int("stationid").unwrap_or(0) as i32;
    let w = &server.world;
    match call.method() {
        "GetLogonData" => logon_data(server, session).await,
        "GetStationItemBits" => Ok(w
            .stations
            .get(&station)
            .map(super::station::station_item_bits)
            .unwrap_or(Value::None)),
        // One office per corporation per station.
        "AreAnyOfficesAvailableForRenting" => {
            let has = db::scalar::<i64>(
                &server.pool,
                r#"SELECT "officeID" FROM game.offices WHERE "corporationID" = $1 AND "stationID" = $2"#,
                &[&corp, &station],
            )
            .await?
            .is_some();
            Ok(Value::from(is_player_corp(corp) && !has))
        }
        "GetQuoteForRentingAnOffice" => Ok(Value::Float(OFFICE_RENT)),
        // RentOffice(cost) -> officeID
        "RentOffice" => {
            if !is_player_corp(corp) || !has_role(server, me, corp, &[]).await? {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            let owner = w
                .stations
                .get(&station)
                .map(|s| s.corporation_id)
                .unwrap_or(1);
            super::wallet::transfer(
                server,
                corp,
                owner,
                OFFICE_RENT,
                "refOfficeRentalFee",
                Some(station as i64),
                "Office rent",
            )
            .await?;
            let folder = super::inventory::create_item(
                server,
                w.type_id("OfficeFolder"),
                corp,
                station as i64,
                w.flag("None"),
                1,
                true,
            )
            .await?;
            db::exec(
                &server.pool,
                r#"INSERT INTO game.offices ("officeID", "corporationID", "stationID") VALUES ($1, $2, $3)"#,
                &[&folder.item_id, &corp, &station],
            )
            .await?;
            let at_station: Vec<i32> = server
                .registry
                .select(None, |c| c.location_id == Some(station as i64))
                .into_iter()
                .filter_map(|(_, c)| c.char_id)
                .collect();
            super::notify_chars(
                server,
                &at_station,
                "OnOfficeRentalChanged",
                vec![
                    Value::Int(corp),
                    Value::from(folder.item_id),
                    Value::from(folder.item_id),
                ],
            );
            super::notify_chars(
                server,
                &at_station,
                "OnCorporationsAtStationChanged",
                vec![Value::Int(corp), Value::Int(1)],
            );
            Ok(Value::from(folder.item_id))
        }
        "MoveCorpHQHere" => {
            if !is_player_corp(corp) || ceo_of(server, corp).await? != Some(me) {
                return Err(user_error("CrpAccessDenied", vec![]));
            }
            db::exec(
                &server.pool,
                r#"UPDATE game.corporations SET "stationID" = $2 WHERE "corporationID" = $1"#,
                &[&corp, &station],
            )
            .await?;
            tell_members(server, corp, "OnCharacterCorporationDataChanged", vec![]).await?;
            Ok(Value::None)
        }
        // Station management is for station-owning corporations; players own none.
        "GetRentableItems" => Ok(rows::rowset(
            &[
                "itemID",
                "typeID",
                "flag",
                "rentedToID",
                "publiclyAvailable",
            ],
            vec![],
        )),
        "GetStationManagementTasks" => Ok(rows::rowset(&["taskID", "taskName"], vec![])),
        "GetStationManagementTaskModifiers" => Ok(rows::rowset(
            &[
                "taskID",
                "discountPerGoodStandingPoint",
                "surchargePerBadStandingPoint",
            ],
            vec![],
        )),
        "GetStationDetails" => Ok(
            match w.stations.get(
                &(call
                    .args()
                    .first()
                    .and_then(Value::as_int)
                    .unwrap_or(station as i64) as i32),
            ) {
                Some(s) => rows::row(
                    &[
                        "stationID",
                        "stationName",
                        "description",
                        "dockingCostPerVolume",
                        "officeRentalCost",
                        "factorySlotRentalCost",
                        "researchFacilitySlotRentalCost",
                    ],
                    vec![
                        Value::Int(s.station_id),
                        db::text_value(&s.name),
                        Value::str(""),
                        Value::Float(0.0),
                        Value::Float(OFFICE_RENT),
                        Value::Float(0.0),
                        Value::Float(0.0),
                    ],
                ),
                None => Value::None,
            },
        ),
        "GetTaskRule" => Ok(rows::row(
            &[
                "minimumStanding",
                "minimumCharSecurity",
                "maximumCharSecurity",
                "minimumCorpSecurity",
                "maximumCorpSecurity",
            ],
            vec![
                Value::Float(-10.0),
                Value::Float(-10.0),
                Value::Float(10.0),
                Value::Float(-10.0),
                Value::Float(10.0),
            ],
        )),
        "UpdateRentableItem"
        | "UpdateStationDetails"
        | "UpdateStationManagementTaskModifier"
        | "UpdateTaskRule"
        => Err(user_error("CrpAccessDenied", vec![])),
        "AreAnyFactorySlotsAvailableForRenting"
        | "AreAnyResearchFacilitySlotsAvailableForRenting"
        | "GetQuoteForRentingAFactorySlot"
        | "GetQuoteForRentingAResearchFacilitySlot"
        | "RentFactorySlot"
        | "RentResearchFacilitySlot"
        | "CancelRentOfFactorySlot" => super::factory::rental_call(server, session, call.method(), call).await,
        m => super::unimplemented("corpStationMgr", m, call),
    }
}
