use crate::db::{now_unix, parse_iso_to_unix, price_key};
use crate::filter::{base_item_id, is_my_guild, non_empty, DEFAULT_GUILD_ID};
use crate::models::{
    AodpHistoryResp, AodpPrice, BattleSummary, BinItem, GameInfoItemData, GuildInfo, KillEvent,
    LocalDb, PriceRecord, StoredBattle,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io::{self, Write};
use std::time::Duration;
use tokio::task::JoinSet;

const GAMEINFO_BASE: &str = "https://gameinfo-sgp.albiononline.com/api/gameinfo";
const AODP_PRICE_BASE: &str = "https://east.albion-online-data.com/api/v2/stats/prices";
const AODP_HISTORY_BASE: &str = "https://east.albion-online-data.com/api/v2/stats/history";
const ITEMS_BULK_URL: &str =
    "https://cdn.jsdelivr.net/gh/ao-data/ao-bin-dumps@master/formatted/items.json";
const ROYAL_LOCATIONS: &str = "Bridgewatch,Fort Sterling,Lymhurst,Martlock,Thetford";

pub async fn refresh_my_alliance(client: &reqwest::Client, db: &mut LocalDb) {
    let url = format!("{GAMEINFO_BASE}/guilds/{DEFAULT_GUILD_ID}");
    let Ok(resp) = client
        .get(url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
    else {
        return;
    };
    if !resp.status().is_success() {
        return;
    }
    if let Ok(info) = resp.json::<GuildInfo>().await {
        db.my_alliance_id = info.alliance_id.filter(|s| !s.is_empty());
        db.my_alliance_name = info.alliance_name.filter(|s| !s.is_empty());
    }
}

pub fn detect_alliance_from_battles(battles: &[StoredBattle], db: &mut LocalDb) {
    if db.my_alliance_id.is_some() || db.manual_alliance.is_some() {
        return;
    }
    for sb in battles {
        if let Some(players) = &sb.summary.players {
            for bp in players.values() {
                if is_my_guild(bp.guild_id.as_deref(), bp.guild_name.as_deref()) {
                    if let Some(aid) = non_empty(bp.alliance_id.as_deref()) {
                        db.my_alliance_id = Some(aid.to_string());
                        db.my_alliance_name =
                            non_empty(bp.alliance_name.as_deref()).map(str::to_string);
                        return;
                    }
                }
            }
        }
        for ev in &sb.events {
            for ep in [&ev.killer, &ev.victim] {
                if is_my_guild(ep.guild_id.as_deref(), ep.guild_name.as_deref()) {
                    if let Some(aid) = non_empty(ep.alliance_id.as_deref()) {
                        db.my_alliance_id = Some(aid.to_string());
                        db.my_alliance_name =
                            non_empty(ep.alliance_name.as_deref()).map(str::to_string);
                        return;
                    }
                }
            }
        }
    }
}

pub async fn get_recent_guild_battles(
    client: &reqwest::Client,
    guild_id: &str,
    limit: usize,
) -> Result<Vec<BattleSummary>, reqwest::Error> {
    let mut all = Vec::new();
    let mut offset = 0usize;
    while all.len() < limit {
        let page_limit = (limit - all.len()).min(50);
        let url = format!("{GAMEINFO_BASE}/battles");
        let batch = client
            .get(url)
            .query(&[
                ("guildId", guild_id),
                ("sort", "recent"),
                ("limit", &page_limit.to_string()),
                ("offset", &offset.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<BattleSummary>>()
            .await?;

        let count = batch.len();
        if count == 0 {
            break;
        }
        all.extend(batch);
        if count < page_limit {
            break;
        }
        offset += count;
    }
    Ok(all)
}

async fn get_battle_detail(
    client: &reqwest::Client,
    battle_id: u64,
) -> Result<BattleSummary, reqwest::Error> {
    let url = format!("{GAMEINFO_BASE}/battles/{battle_id}");
    client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<BattleSummary>()
        .await
}

async fn get_battle_events(
    client: &reqwest::Client,
    battle_id: u64,
) -> Result<Vec<KillEvent>, reqwest::Error> {
    let mut events = Vec::new();
    let mut offset = 0usize;
    let limit = 51usize;

    loop {
        let url = format!("{GAMEINFO_BASE}/events/battle/{battle_id}");
        let batch = client
            .get(url)
            .query(&[("offset", offset), ("limit", limit)])
            .send()
            .await?
            .error_for_status()?
            .json::<Vec<KillEvent>>()
            .await?;

        let count = batch.len();
        if count == 0 {
            break;
        }
        events.extend(batch);
        if count < limit {
            break;
        }
        offset += limit;
    }

    events.sort_by(|a, b| a.time_stamp.cmp(&b.time_stamp));
    Ok(events)
}

pub async fn load_or_fetch_battle(
    client: &reqwest::Client,
    battle_id: u64,
    db: &mut LocalDb,
    force_refresh: bool,
) -> Result<(StoredBattle, bool), Box<dyn Error>> {
    if !force_refresh {
        if let Some(stored) = db.battles.get(&battle_id) {
            let is_complete = !stored.events.is_empty()
                && (stored.events.len() as u64 >= stored.summary.total_kills
                    || stored.events.len() >= 1000);
            if is_complete {
                return Ok((stored.clone(), true));
            }
        }
    }

    match (
        get_battle_detail(client, battle_id).await,
        get_battle_events(client, battle_id).await,
    ) {
        (Ok(summary), Ok(events)) => {
            // 保留已有的价格快照，避免 -r 强刷事件时意外丢失原价格快照
            let (old_snap, old_snap_at) = db
                .battles
                .get(&battle_id)
                .map(|b| (b.price_snapshot.clone(), b.price_snapshot_at))
                .unwrap_or_default();
            let stored = StoredBattle {
                summary,
                events,
                price_snapshot: old_snap,
                price_snapshot_at: old_snap_at,
            };
            db.battles.insert(battle_id, stored.clone());
            Ok((stored, false))
        }
        (Err(e), _) | (_, Err(e)) => {
            if let Some(stored) = db.battles.get(&battle_id) {
                eprintln!("⚠️ 官服查询 Battle {battle_id} 失败({e})，改用本地离线存档。");
                Ok((stored.clone(), true))
            } else {
                Err(Box::new(e))
            }
        }
    }
}

async fn try_preload_bulk_zh_names(client: &reqwest::Client, db: &mut LocalDb) -> usize {
    if db.item_names_zh.len() >= 1000 {
        return 0;
    }
    eprint!("  [联网] 尝试预加载中文词典...");
    let _ = io::stderr().flush();
    let Ok(resp) = client
        .get(ITEMS_BULK_URL)
        .timeout(Duration::from_secs(12))
        .send()
        .await
    else {
        eprintln!(" 跳过(超时/不可达)，改为按需抓取。");
        return 0;
    };
    if !resp.status().is_success() {
        eprintln!(" 跳过(HTTP {})，改为按需抓取。", resp.status());
        return 0;
    }
    let Ok(text) = resp.text().await else {
        eprintln!(" 跳过，改为按需抓取。");
        return 0;
    };
    let clean = text.trim_start_matches('\u{FEFF}');
    let Ok(items) = serde_json::from_str::<Vec<BinItem>>(clean) else {
        eprintln!(" 跳过，改为按需抓取。");
        return 0;
    };

    let mut added = 0usize;
    for it in items {
        let Some(unique) = it.unique_name else {
            continue;
        };
        let Some(names) = it.localized_names else {
            continue;
        };
        let zh = names
            .get("ZH-CN")
            .and_then(|v| v.as_deref())
            .or_else(|| names.get("ZH-TW").and_then(|v| v.as_deref()))
            .or_else(|| names.get("EN-US").and_then(|v| v.as_deref()));
        if let Some(name) = zh.filter(|s| !s.is_empty()) {
            let base = base_item_id(&unique).to_string();
            if !db.item_names_zh.contains_key(&base) {
                db.item_names_zh.insert(base, name.to_string());
                added += 1;
            }
        }
    }
    eprintln!(" 完成(+{added}条)");
    added
}

async fn fetch_single_item_zh(client: &reqwest::Client, item_id: &str) -> Option<String> {
    let base = base_item_id(item_id);
    let candidates = if base != item_id {
        vec![base.to_string(), item_id.to_string()]
    } else {
        vec![base.to_string()]
    };

    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(600 * attempt as u64)).await;
        }
        for id in &candidates {
            let url = format!("{GAMEINFO_BASE}/items/{id}/data");
            let Ok(resp) = client
                .get(url)
                .timeout(Duration::from_secs(15))
                .send()
                .await
            else {
                continue;
            };
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            if !resp.status().is_success() {
                continue;
            }
            let Ok(data) = resp.json::<GameInfoItemData>().await else {
                continue;
            };
            if let Some(names) = data.localized_names {
                let zh = names
                    .get("ZH-CN")
                    .and_then(|v| v.as_deref())
                    .or_else(|| names.get("ZH-TW").and_then(|v| v.as_deref()))
                    .or_else(|| names.get("EN-US").and_then(|v| v.as_deref()));
                if let Some(name) = zh.filter(|s| !s.is_empty()) {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

pub async fn ensure_zh_names(
    client: &reqwest::Client,
    item_ids: &[String],
    db: &mut LocalDb,
) -> usize {
    let mut added = try_preload_bulk_zh_names(client, db).await;

    let missing_bases: Vec<String> = item_ids
        .iter()
        .map(|id| base_item_id(id).to_string())
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|b| !db.item_names_zh.contains_key(b))
        .collect();

    if missing_bases.is_empty() {
        return added;
    }

    let total = missing_bases.len();
    let mut done = 0usize;
    for (idx, chunk) in missing_bases.chunks(2).enumerate() {
        if idx > 0 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let mut set = JoinSet::new();
        for base in chunk {
            let c = client.clone();
            let b = base.clone();
            set.spawn(async move {
                let zh = fetch_single_item_zh(&c, &b).await;
                (b, zh)
            });
        }
        while let Some(Ok((base, maybe_zh))) = set.join_next().await {
            done += 1;
            if let Some(zh) = maybe_zh {
                db.item_names_zh.insert(base, zh);
                added += 1;
            }
        }
        eprint!("\r  [联网] 补全本场缺失中文名: {done}/{total}");
        let _ = io::stderr().flush();
    }
    eprintln!();
    added
}

fn pick_royal_buy_price(entries: &[(u64, Option<u64>)], now: u64) -> Option<u64> {
    let cutoff_72h = now.saturating_sub(72 * 3600);
    let fresh: Vec<u64> = entries
        .iter()
        .filter_map(|&(p, ts)| {
            if p > 0 && ts.map_or(false, |t| t >= cutoff_72h) {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    let mut pool = if !fresh.is_empty() {
        fresh
    } else {
        entries
            .iter()
            .filter_map(|&(p, _)| if p > 0 { Some(p) } else { None })
            .collect()
    };

    if pool.is_empty() {
        return None;
    }
    pool.sort_unstable();
    let median = pool[pool.len() / 2];
    let realistic_min = pool
        .iter()
        .copied()
        .find(|&p| p >= median / 4)
        .unwrap_or(median);
    Some(realistic_min)
}

pub async fn fetch_prices_and_recent_trades(
    client: &reqwest::Client,
    target_ids: &[String],
    db: &mut LocalDb,
    live_prices: &mut HashMap<(String, u8), u64>,
    recent_trades: &mut HashMap<(String, u8), u64>,
) -> Result<(usize, usize), Box<dyn Error>> {
    let unique_ids: Vec<String> = target_ids
        .iter()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    if unique_ids.is_empty() {
        return Ok((0, 0));
    }

    let now = now_unix();
    let mut live_grouped: HashMap<(String, u8), Vec<(u64, Option<u64>)>> = HashMap::new();
    let mut rate_limited = false;

    let chunks: Vec<&[String]> = unique_ids.chunks(60).collect();
    let total_chunks = chunks.len();
    for (ci, chunk) in chunks.into_iter().enumerate() {
        eprint!(
            "\r  [联网] 查询皇家城实时买价: 第 {}/{} 批",
            ci + 1,
            total_chunks
        );
        let _ = io::stderr().flush();

        let ids_param = chunk.join(",");
        let url = format!("{AODP_PRICE_BASE}/{ids_param}.json");
        let resp = client
            .get(url)
            .query(&[("locations", ROYAL_LOCATIONS), ("qualities", "1,2,3,4,5")])
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let reset = resp
                .headers()
                .get("RateLimit-Reset")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("未知");
            eprintln!("\n⚠️ 实时物价触发 429 (Reset: {reset})，自动改用本地数据库。");
            rate_limited = true;
            break;
        }

        let records = resp.error_for_status()?.json::<Vec<AodpPrice>>().await?;
        for r in records {
            if r.sell_price_min > 0 {
                let ts = r.sell_price_min_date.as_deref().and_then(parse_iso_to_unix);
                live_grouped
                    .entry((r.item_id, r.quality))
                    .or_default()
                    .push((r.sell_price_min, ts));
            }
        }
    }
    if !rate_limited {
        eprintln!();
    }

    let mut updated_count = 0usize;
    for (key, entries) in live_grouped {
        let Some(buy_price) = pick_royal_buy_price(&entries, now) else {
            continue;
        };
        live_prices.insert(key.clone(), buy_price);

        let k_str = price_key(&key.0, key.1);
        let is_manual = db
            .prices
            .get(&k_str)
            .map(|r| r.source == "manual")
            .unwrap_or(false);
        if !is_manual {
            db.prices.insert(
                k_str,
                PriceRecord {
                    price: buy_price,
                    source: "live".to_string(),
                    updated_at: now,
                    tag: None,
                },
            );
            updated_count += 1;
        }
    }

    let mut trade_added = 0usize;
    if !rate_limited {
        let need_hist_ids: Vec<String> = unique_ids
            .into_iter()
            .filter(|id| !(1u8..=3).any(|q| live_prices.contains_key(&(id.clone(), q))))
            .collect();

        let h_chunks: Vec<&[String]> = need_hist_ids.chunks(40).collect();
        let total_h = h_chunks.len();
        for (hi, chunk) in h_chunks.into_iter().enumerate() {
            eprint!(
                "\r  [联网] 补充 48h 近期成交均价: 第 {}/{} 批",
                hi + 1,
                total_h
            );
            let _ = io::stderr().flush();

            let ids_param = chunk.join(",");
            let url = format!("{AODP_HISTORY_BASE}/{ids_param}.json");
            let resp = client
                .get(url)
                .query(&[
                    ("locations", ROYAL_LOCATIONS),
                    ("qualities", "1,2,3,4,5"),
                    ("time-scale", "6"),
                ])
                .send()
                .await?;

            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                eprintln!("\n⚠️ 历史成交接口触发 429，跳过剩余批次。");
                break;
            }
            if !resp.status().is_success() {
                continue;
            }

            let Ok(hist_list) = resp.json::<Vec<AodpHistoryResp>>().await else {
                continue;
            };

            let mut points_by_item: HashMap<(String, u8), Vec<(u64, u64, f64)>> = HashMap::new();
            for h in hist_list {
                for pt in h.data {
                    if pt.item_count > 0 && pt.avg_price > 0.0 {
                        let ts = pt
                            .timestamp
                            .as_deref()
                            .and_then(parse_iso_to_unix)
                            .unwrap_or(0);
                        points_by_item
                            .entry((h.item_id.clone(), h.quality))
                            .or_default()
                            .push((ts, pt.item_count, pt.avg_price));
                    }
                }
            }

            for (key, mut pts) in points_by_item {
                pts.sort_by(|a, b| b.0.cmp(&a.0));
                let newest_ts = pts[0].0;
                let window_start = newest_ts.saturating_sub(48 * 3600);

                let mut total_vol = 0u64;
                let mut total_val = 0f64;
                for &(ts, count, avg_p) in pts.iter().take(16) {
                    if ts >= window_start || total_vol == 0 {
                        total_vol += count;
                        total_val += avg_p * (count as f64);
                    }
                }
                if total_vol == 0 {
                    continue;
                }
                let weighted_price = (total_val / (total_vol as f64)).round() as u64;
                recent_trades.insert(key.clone(), weighted_price);

                let k_str = price_key(&key.0, key.1);
                let is_manual = db
                    .prices
                    .get(&k_str)
                    .map(|r| r.source == "manual")
                    .unwrap_or(false);
                if !is_manual {
                    db.prices.insert(
                        k_str,
                        PriceRecord {
                            price: weighted_price,
                            source: "recent_trade".to_string(),
                            updated_at: now,
                            tag: None,
                        },
                    );
                    trade_added += 1;
                }
            }
        }
        if total_h > 0 {
            eprintln!();
        }
    }

    Ok((updated_count, trade_added))
}
