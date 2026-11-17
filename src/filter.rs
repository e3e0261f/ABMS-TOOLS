use crate::models::{LocalDb, Relation};

pub const DEFAULT_GUILD_ID: &str = "gULQqiahQ8eqsL6tz-Kl_Q";
pub const DEFAULT_GUILD_NAME: &str = "a brave man";

pub fn non_empty(opt: Option<&str>) -> Option<&str> {
    opt.map(str::trim).filter(|s| !s.is_empty())
}

pub fn is_my_guild(guild_id: Option<&str>, guild_name: Option<&str>) -> bool {
    if guild_id == Some(DEFAULT_GUILD_ID) {
        return true;
    }
    if let Some(name) = guild_name {
        return name.eq_ignore_ascii_case(DEFAULT_GUILD_NAME);
    }
    false
}

/// 过滤掉不可交易物品（任务代币、绑定书、垃圾等），不计入缺价统计
pub fn is_untradable_item(item_id: &str) -> bool {
    item_id.contains("_NONTRADABLE")
        || item_id.starts_with("QUESTITEM_")
        || item_id.ends_with("_TRASH")
        || item_id.starts_with("UNIQUE_")
}

pub fn base_item_id(item_id: &str) -> &str {
    item_id.split('@').next().unwrap_or(item_id)
}

pub fn parse_tier_and_enchant(item_id: &str) -> (u8, u8) {
    let tier = if item_id.starts_with('T') {
        item_id
            .as_bytes()
            .get(1)
            .and_then(|&b| (b as char).to_digit(10))
            .map(|d| d as u8)
            .filter(|&t| (1..=8).contains(&t))
            .unwrap_or(0)
    } else {
        0
    };

    let enchant = item_id
        .rsplit_once('@')
        .and_then(|(_, e)| e.parse::<u8>().ok())
        .unwrap_or(0);

    (tier, enchant)
}

/// 同等效阶位兄弟装备（例如 6.3 -> 5.4, 7.2, 8.1）
pub fn equivalent_tier_siblings(item_id: &str) -> (u8, Vec<String>) {
    let (tier, enchant) = parse_tier_and_enchant(item_id);
    if tier < 4 || is_untradable_item(item_id) {
        return (0, Vec::new());
    }
    let base = base_item_id(item_id);
    let Some(suffix) = base.get(3..) else {
        return (0, Vec::new());
    };
    if !base.starts_with(&format!("T{tier}_")) {
        return (0, Vec::new());
    }
    if suffix.starts_with("MEAL_") || suffix.starts_with("POTION_") || suffix.starts_with("MOUNT_")
    {
        return (0, Vec::new());
    }

    let equiv = tier + enchant;
    let mut siblings = Vec::new();
    for t in 4u8..=8 {
        if let Some(e) = equiv.checked_sub(t) {
            let max_e = if enchant >= 4 { 4 } else { 3 };
            if e <= max_e && !(t == tier && e == enchant) {
                let id = if e == 0 {
                    format!("T{t}_{suffix}")
                } else {
                    format!("T{t}_{suffix}@{e}")
                };
                siblings.push(id);
            }
        }
    }
    (equiv, siblings)
}

pub fn classify_relation(
    guild_id: Option<&str>,
    guild_name: Option<&str>,
    alliance_id: Option<&str>,
    alliance_name: Option<&str>,
    db: &LocalDb,
) -> Relation {
    if is_my_guild(guild_id, guild_name) {
        return Relation::Ours;
    }

    // 1. 检查手动设置的友军公会
    if let Some(gname) = non_empty(guild_name) {
        if db.ally_guilds.iter().any(|g| g.eq_ignore_ascii_case(gname)) {
            return Relation::Ally;
        }
    }

    // 2. 检查手动指定的主联盟 + 友军联盟白名单
    let mut my_alliance_names: Vec<&str> = Vec::new();
    if let Some(m) = non_empty(db.manual_alliance.as_deref()) {
        my_alliance_names.push(m);
    } else if let Some(a) = non_empty(db.my_alliance_name.as_deref()) {
        my_alliance_names.push(a);
    }
    for extra in &db.ally_alliances {
        if let Some(s) = non_empty(Some(extra.as_str())) {
            my_alliance_names.push(s);
        }
    }

    if let Some(aname) = non_empty(alliance_name) {
        if my_alliance_names
            .iter()
            .any(|m| m.eq_ignore_ascii_case(aname))
        {
            return Relation::Ally;
        }
    }
    if let Some(aid) = non_empty(alliance_id) {
        if db.manual_alliance.as_deref() == Some(aid)
            || (db.manual_alliance.is_none() && db.my_alliance_id.as_deref() == Some(aid))
            || db.ally_alliances.iter().any(|x| x == aid)
        {
            return Relation::Ally;
        }
    }

    Relation::Hostile
}

/// 返回 (分组键, 显示名称)：无联盟按阵营严格拆开，绝不混入敌对与我方
pub fn alliance_bucket(alliance_name: Option<&str>, rel: Relation) -> (String, String) {
    if let Some(name) = non_empty(alliance_name) {
        return (format!("A:{name}"), name.to_string());
    }
    match rel {
        Relation::Ours => ("[无联盟:我方]".to_string(), "[无联盟-我方]".to_string()),
        Relation::Ally => ("[无联盟:友盟]".to_string(), "[无联盟-友盟]".to_string()),
        Relation::Hostile => ("[无联盟:敌对]".to_string(), "[无联盟-敌对]".to_string()),
    }
}
