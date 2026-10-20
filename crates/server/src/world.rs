//! In-memory view of the static world, loaded once at startup.

use std::collections::HashMap;

use anyhow::{Context, Result};
use deadpool_postgres::Pool;
use marshal::Value;

use crate::rows;

#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub group_id: i32,
    pub category_id: i32,
    pub name: String,
    pub volume: f64,
    pub capacity: f64,
}

#[derive(Debug, Clone)]
pub struct Station {
    pub station_id: i32,
    pub type_id: i32,
    pub corporation_id: i32,
    pub solar_system_id: i32,
    pub constellation_id: i32,
    pub region_id: i32,
    pub name: String,
    pub service_mask: i32,
    pub hangar_graphic_id: i32,
    pub pos: [f64; 3],
}

#[derive(Debug, Clone)]
pub struct System {
    pub constellation_id: i32,
    pub region_id: i32,
    pub name: String,
    pub security: f64,
    pub faction_id: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct Bloodline {
    pub race_id: i32,
    pub base: [i32; 5],
    pub skills: Vec<i32>,
    pub ship_type_id: Option<i32>,
}

/// Creation rows keyed by ID, for validating CreateCharacter.
#[derive(Debug, Default)]
pub struct Creation {
    pub race_skill: HashMap<i32, i32>,
    pub bloodlines: HashMap<i32, Bloodline>,
    /// ancestryID -> (bloodlineID, post attributes in ATTRIBUTE order, skills)
    pub ancestries: HashMap<i32, (i32, [i32; 5], Vec<i32>)>,
    /// schoolID -> (raceID, corporationID, stationID)
    pub schools: HashMap<i32, (i32, i32, i32)>,
    /// departmentID -> (schoolID, skills)
    pub departments: HashMap<i32, (i32, Vec<i32>)>,
    /// fieldID -> (departmentID, skills, equipment)
    pub fields: HashMap<i32, (i32, Vec<i32>, Option<(i32, i32)>)>,
    /// specialityID -> (fieldID, skills, equipment)
    pub specialities: HashMap<i32, (i32, Vec<i32>, Option<(i32, i32)>)>,
    /// const.chrattr* in Charisma, Intelligence, Memory, Perception, Willpower order
    pub attribute_ids: [i32; 5],
    /// The prebuilt GetCharCreationInfo / GetAppearanceInfo replies.
    pub info: Value,
    pub appearance_info: Value,
}

pub const ATTRIBUTE_NAMES: [&str; 5] = [
    "Charisma",
    "Intelligence",
    "Memory",
    "Perception",
    "Willpower",
];

pub struct World {
    pub types: HashMap<i32, TypeInfo>,
    pub stations: HashMap<i32, Station>,
    /// stationTypeID -> raceID (corp.GetPublicStationInfo().raceID)
    pub station_race: HashMap<i32, i32>,
    pub systems: HashMap<i32, System>,
    pub constants: HashMap<String, i32>,
    /// static.flags, keyed by name without spaces ('Hangar', 'LoSlot0', ...)
    pub flags: HashMap<String, i32>,
    /// static.groups by name without spaces
    pub groups: HashMap<String, i32>,
    /// categoryName without spaces -> categoryID.
    pub categories: HashMap<String, i32>,
    /// raceName -> raceID (const.race*).
    pub races: HashMap<String, i32>,
    /// dgmattribs attributeName -> attributeID
    pub attributes: HashMap<String, i32>,
    pub creation: Creation,
}

impl World {
    pub async fn load(pool: &Pool) -> Result<World> {
        let c = pool.get().await?;
        let mut types = HashMap::new();
        for r in c
            .query(
                r#"SELECT t."typeID", t."groupID", g."categoryID", t."typeName", t."volume", t."capacity"
                   FROM static.types t JOIN static.groups g USING ("groupID")"#,
                &[],
            )
            .await?
        {
            types.insert(
                r.get(0),
                TypeInfo { group_id: r.get(1), category_id: r.get(2), name: r.get(3), volume: r.get(4), capacity: r.get(5) },
            );
        }
        let mut stations = HashMap::new();
        for r in c
            .query(
                r#"SELECT s."stationID", s."stationTypeID", s."corporationID", s."solarSystemID", s."constellationID",
                          s."regionID", s."stationName", s."serviceMask", st."hangarGraphicID", s."x", s."y", s."z"
                   FROM static.stations s JOIN static.station_types st USING ("stationTypeID")"#,
                &[],
            )
            .await?
        {
            stations.insert(
                r.get(0),
                Station {
                    station_id: r.get(0),
                    type_id: r.get(1),
                    corporation_id: r.get(2),
                    solar_system_id: r.get(3),
                    constellation_id: r.get(4),
                    region_id: r.get(5),
                    name: r.get(6),
                    service_mask: r.get(7),
                    hangar_graphic_id: r.get(8),
                    pos: [r.get(9), r.get(10), r.get(11)],
                },
            );
        }
        let station_race = c
            .query(r#"SELECT "stationTypeID", "raceID" FROM static.station_types WHERE "raceID" IS NOT NULL"#, &[])
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let mut systems = HashMap::new();
        for r in c
            .query(
                r#"SELECT s."itemID", s."locationID", c."locationID", s."itemName", s."security", s."factionID"
                   FROM static.map_objects s JOIN static.map_objects c ON c."itemID" = s."locationID"
                   WHERE s."groupID" = 5"#,
                &[],
            )
            .await?
        {
            systems.insert(
                r.get(0),
                System {
                    constellation_id: r.get(1),
                    region_id: r.get(2),
                    name: r.get(3),
                    security: r.get::<_, Option<f64>>(4).unwrap_or(0.0),
                    faction_id: r.get(5),
                },
            );
        }
        let constants = c
            .query(
                r#"SELECT "constantID", "constantValue" FROM static.constants"#,
                &[],
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let flags = c
            .query(r#"SELECT "flagName", "flagID" FROM static.flags"#, &[])
            .await?
            .iter()
            .map(|r| (r.get::<_, String>(0).replace(' ', ""), r.get(1)))
            .collect();
        let groups = c
            .query(r#"SELECT "groupName", "groupID" FROM static.groups"#, &[])
            .await?
            .iter()
            .map(|r| (r.get::<_, String>(0).replace(' ', ""), r.get(1)))
            .collect();
        let categories = c
            .query(
                r#"SELECT "categoryName", "categoryID" FROM static.categories"#,
                &[],
            )
            .await?
            .iter()
            .map(|r| (r.get::<_, String>(0).replace(' ', ""), r.get(1)))
            .collect();
        let races = c
            .query(r#"SELECT "raceName", "raceID" FROM static.races"#, &[])
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let attributes = c
            .query(
                r#"SELECT "attributeName", "attributeID" FROM static.dgmattribs"#,
                &[],
            )
            .await?
            .iter()
            .map(|r| (r.get(0), r.get(1)))
            .collect();
        let creation = load_creation(&c)
            .await
            .context("character creation content")?;
        tracing::info!(
            types = types.len(),
            stations = stations.len(),
            systems = systems.len(),
            "world loaded"
        );
        Ok(World {
            types,
            stations,
            station_race,
            systems,
            constants,
            flags,
            groups,
            categories,
            races,
            attributes,
            creation,
        })
    }

    /// A type's ID by its const name ('Bookmark' for const.typeBookmark):
    /// the type name with spaces removed (InsertConstantsFromRowset).
    pub fn type_id(&self, name: &str) -> i32 {
        self.types
            .iter()
            .find(|(_, t)| t.name.replace(' ', "") == name)
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("type {name} missing from static.types"))
    }

    pub fn constant(&self, name: &str) -> i32 {
        *self
            .constants
            .get(name)
            .unwrap_or_else(|| panic!("constant {name} missing from static.constants"))
    }

    /// A group's ID by its name without spaces ('Stargate', 'SolarSystem', ...).
    pub fn group_id(&self, name: &str) -> Option<i32> {
        self.groups.get(name).copied()
    }

    /// A category's ID by its const name ('Ship' for const.categoryShip).
    pub fn category_id(&self, name: &str) -> i32 {
        *self
            .categories
            .get(name)
            .unwrap_or_else(|| panic!("category {name} missing from static.categories"))
    }

    pub fn flag(&self, name: &str) -> i32 {
        *self
            .flags
            .get(name)
            .unwrap_or_else(|| panic!("flag {name} missing from static.flags"))
    }

    pub fn attribute(&self, name: &str) -> Option<i32> {
        self.attributes.get(name).copied()
    }

    pub fn type_info(&self, type_id: i32) -> Option<&TypeInfo> {
        self.types.get(&type_id)
    }
}

fn skills(r: &tokio_postgres::Row, cols: &[&str]) -> Vec<i32> {
    cols.iter()
        .filter_map(|c| r.get::<_, Option<i32>>(*c))
        .collect()
}

fn equipment(r: &tokio_postgres::Row) -> Option<(i32, i32)> {
    Some((
        r.get::<_, Option<i32>>("typeID")?,
        r.get::<_, Option<i32>>("typeQuantity").unwrap_or(1),
    ))
}

async fn rowset_of(c: &deadpool_postgres::Object, sql: &str) -> Result<Value> {
    let stmt = c.prepare(sql).await?;
    let rows = c.query(&stmt, &[]).await?;
    let header: Vec<&str> = stmt.columns().iter().map(|c| c.name()).collect();
    rows::rowset_from(&header, &rows)
}

async fn load_creation(c: &deadpool_postgres::Object) -> Result<Creation> {
    let mut cr = Creation::default();
    for r in c
        .query(r#"SELECT "raceID", "skillTypeID1" FROM static.races"#, &[])
        .await?
    {
        if let Some(s) = r.get::<_, Option<i32>>(1) {
            cr.race_skill.insert(r.get(0), s);
        }
    }
    for r in c.query(r#"SELECT * FROM static.bloodlines"#, &[]).await? {
        cr.bloodlines.insert(
            r.get("bloodlineID"),
            Bloodline {
                race_id: r.get("raceID"),
                base: ATTRIBUTE_NAMES.map(|a| r.get::<_, i32>(a.to_lowercase().as_str())),
                skills: skills(&r, &["skillTypeID1", "skillTypeID2"]),
                ship_type_id: r.get("shipTypeID"),
            },
        );
    }
    for r in c
        .query(r#"SELECT * FROM static.cc_ancestries"#, &[])
        .await?
    {
        cr.ancestries.insert(
            r.get("ancestryID"),
            (
                r.get("bloodlineID"),
                ATTRIBUTE_NAMES.map(|a| r.get::<_, i32>(format!("post{a}").as_str())),
                skills(&r, &["skillTypeID1", "skillTypeID2"]),
            ),
        );
    }
    for r in c
        .query(
            r#"SELECT "schoolID", "raceID", "corporationID", "stationID" FROM static.cc_schools"#,
            &[],
        )
        .await?
    {
        cr.schools.insert(r.get(0), (r.get(1), r.get(2), r.get(3)));
    }
    for r in c
        .query(r#"SELECT * FROM static.cc_departments"#, &[])
        .await?
    {
        cr.departments.insert(
            r.get("departmentID"),
            (
                r.get("schoolID"),
                skills(&r, &["skillTypeID1", "skillTypeID2", "skillTypeID3"]),
            ),
        );
    }
    for r in c.query(r#"SELECT * FROM static.cc_fields"#, &[]).await? {
        cr.fields.insert(
            r.get("fieldID"),
            (
                r.get("departmentID"),
                skills(&r, &["skillTypeID1", "skillTypeID2"]),
                equipment(&r),
            ),
        );
    }
    for r in c
        .query(r#"SELECT * FROM static.cc_specialities"#, &[])
        .await?
    {
        cr.specialities.insert(
            r.get("specialityID"),
            (
                r.get("fieldID"),
                skills(&r, &["skillTypeID1", "skillTypeID2"]),
                equipment(&r),
            ),
        );
    }
    for (i, a) in ATTRIBUTE_NAMES.iter().enumerate() {
        let r = c
            .query_one(
                r#"SELECT "attributeID" FROM static.cc_attributes WHERE "attributeName" = $1"#,
                &[a],
            )
            .await?;
        cr.attribute_ids[i] = r.get(0);
    }

    // GetCharCreationInfo: a dict whose keys ccutil reads as attributes.
    let mut info = vec![
        // Only races with bloodlines are playable: face.py GetRaceName/GetBloodlineRace
        // know just Amarr, Caldari, Gallente and Minmatar, so a Jove pick has no face.
        (
            "races",
            rowset_of(
                c,
                r#"SELECT * FROM static.races WHERE "raceID" IN
            (SELECT "raceID" FROM static.bloodlines) ORDER BY "raceID""#,
            )
            .await?,
        ),
        (
            "bloodlines",
            rowset_of(
                c,
                r#"SELECT * FROM static.bloodlines ORDER BY "bloodlineID""#,
            )
            .await?,
        ),
        (
            "ancestries",
            rowset_of(
                c,
                r#"SELECT * FROM static.cc_ancestries ORDER BY "ancestryID""#,
            )
            .await?,
        ),
        (
            "schools",
            rowset_of(c, r#"SELECT * FROM static.cc_schools ORDER BY "schoolID""#).await?,
        ),
        (
            "departments",
            rowset_of(
                c,
                r#"SELECT * FROM static.cc_departments ORDER BY "departmentID""#,
            )
            .await?,
        ),
        (
            "fields",
            rowset_of(c, r#"SELECT * FROM static.cc_fields ORDER BY "fieldID""#).await?,
        ),
        (
            "specialities",
            rowset_of(
                c,
                r#"SELECT * FROM static.cc_specialities ORDER BY "specialityID""#,
            )
            .await?,
        ),
        (
            "attributes",
            rowset_of(
                c,
                r#"SELECT * FROM static.cc_attributes ORDER BY "attributeID""#,
            )
            .await?,
        ),
        (
            "benefits",
            rows::rowset(&["benefitID", "benefitName", "description"], vec![]),
        ),
        (
            "handicaps",
            rows::rowset(&["handicapID", "handicapName", "description"], vec![]),
        ),
    ];
    // face.py allOptions / stems. Option families: (bloodlineID, gender, <stem>ID).
    // lights/backgrounds: (<stem>ID) only (ccutil.GetAppearanceOptions).
    const FAMILIES: [(&str, &str); 12] = [
        ("skins", "skin"),
        ("eyes", "eyes"),
        ("eyebrows", "eyebrows"),
        ("makeups", "makeup"),
        ("lipsticks", "lipstick"),
        ("decos", "deco"),
        ("hairs", "hair"),
        ("beards", "beard"),
        ("accessories", "accessory"),
        ("costumes", "costume"),
        ("lights", "light"),
        ("backgrounds", "background"),
    ];
    let mut appearance = Vec::new();
    for (family, stem) in FAMILIES {
        let id_col = format!("{stem}ID");
        let name_col = format!("{stem}Name");
        if family == "lights" || family == "backgrounds" {
            let rows = c
                .query(r#"SELECT "id" FROM static.cc_appearance_names WHERE "family" = $1 ORDER BY "id""#, &[&family])
                .await?;
            info.push((family, rows::rowset_from(&[&id_col], &rows)?));
        } else {
            let rows = c
                .query(
                    r#"SELECT "bloodlineID", "gender", "id" FROM static.cc_appearance_options
                       WHERE "family" = $1 ORDER BY "bloodlineID", "gender", "id""#,
                    &[&family],
                )
                .await?;
            info.push((
                family,
                rows::rowset_from(&["bloodlineID", "gender", &id_col], &rows)?,
            ));
        }
        // GetAppearanceInfo: family -> rows of (<stem>ID, <stem>Name). ccutil
        // derives the stem from the first row's attributes, so every family
        // must use the face.py stem names.
        let rows = c
            .query(r#"SELECT "id", "name" FROM static.cc_appearance_names WHERE "family" = $1 ORDER BY "id""#, &[&family])
            .await?;
        appearance.push((
            Value::str(family),
            rows::rowset_from(&[&id_col, &name_col], &rows)?,
        ));
    }
    cr.info = Value::Dict(info.into_iter().map(|(k, v)| (Value::str(k), v)).collect());
    cr.appearance_info = Value::Dict(appearance);
    Ok(cr)
}
