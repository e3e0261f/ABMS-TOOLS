use crate::api::{
    detect_alliance_from_battles, ensure_zh_names, fetch_prices_and_recent_trades,
    load_or_fetch_battle, refresh_my_alliance,
};
use crate::db::{db_path, now_unix, save_db, short_unix_time};
use crate::filter::{
    alliance_bucket, classify_relation, equivalent_tier_siblings, is_my_guild, is_untradable_item,
    non_empty, DEFAULT_GUILD_NAME,
};
use crate::models::{
    AllianceHealAgg, AllianceLossAgg, BattleSummary, CombatantStats, DeathRecord, Equipment,
    EvaluatedEvent, GearCost, GuildLossAgg, GuildMemberStats, KillEvent, LocalDb, PriceRecord,
    Relation, StoredBattle,
};
use crate::pricing::{
    build_battle_snapshot, evaluate_equipment, evaluate_inventory, lookup_exact_or_lower_quality,
};
use crate::theme::{
    apply_relation_color, cell_r, fmt_num, format_cost_cell, format_cost_text, format_player_tag,
    hdr, item_cell, killboard_url, new_clean_table, osc8_link, player_link, relation_cell,
    short_hms, short_time,
};
use comfy_table::{Attribute, Cell, Color};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io::{self, Write};

fn update_weapon_ip(entry: &mut GuildMemberStats, eq: Option<&Equipment>, avg_ip: Option<f64>) {
    if entry.main_hand.is_none() {
        if let Some(mh) = eq
            .and_then(|e| e.main_hand.as_ref())
            .and_then(|i| i.item_type.as_ref())
        {
            entry.main_hand = Some(mh.clone());
        }
    }
    if entry.avg_ip.is_none() {
        if let Some(ip) = avg_ip.filter(|&v| v > 0.0) {
            entry.avg_ip = Some(ip);
        }
    }
}

fn print_gear_table(title: Option<&str>, gears: &[GearCost], zh_names: &HashMap<String, String>) {
    if gears.is_empty() {
        return;
    }
    if let Some(t) = title {
        println!("  {t}");
    }
    let mut gear_table = new_clean_table();
    gear_table.set_header(vec![
        hdr("槽位", false),
        hdr("分类", false),
        hdr("装备名称", false),
        hdr("品质", true),
        hdr("数量", true),
        hdr("估价", true),
    ]);

    for g in gears {
        let cat = if g.is_cross { "十字架" } else { "其他" };
        let price_cell = if g.is_untradable {
            cell_r("非卖品").fg(Color::DarkGrey)
        } else if g.is_missing || g.subtotal == 0 {
            cell_r("无报价")
                .fg(Color::Red)
                .add_attribute(Attribute::Bold)
        } else {
            let txt = if g.source_tag.is_empty() {
                fmt_num(g.subtotal)
            } else {
                format!("{} {}", fmt_num(g.subtotal), g.source_tag)
            };
            let c = cell_r(txt);
            if g.is_approx {
                c.fg(Color::Rgb {
                    r: 215,
                    g: 190,
                    b: 110,
                })
            } else {
                c
            }
        };

        gear_table.add_row(vec![
            Cell::new(g.slot),
            Cell::new(cat),
            item_cell(&g.item_id, zh_names),
            cell_r(format!("Q{}", g.quality)),
            cell_r(format!("x{}", g.count)),
            price_cell,
        ]);
    }
    println!("{gear_table}");
}

pub fn print_battle_list(battles: &[BattleSummary], db: &LocalDb) {
    println!(
        "\n公会「{}」最近 {} 场战斗（-n 20 查更多 | -u 更新物价 | -s 历史总榜）\n",
        DEFAULT_GUILD_NAME,
        battles.len()
    );

    let mut table = new_clean_table();
    table.set_header(vec![
        hdr("序号", true),
        hdr("Battle ID", false),
        hdr("开始时间", false),
        hdr("总击杀", true),
        hdr("我方参战", true),
        hdr("我方阵亡", true),
        hdr("本地库 / 快照", false),
    ]);

    for (idx, b) in battles.iter().enumerate() {
        let mut our_count = 0u32;
        let mut our_deaths = 0u32;
        if let Some(players) = &b.players {
            for bp in players.values() {
                if is_my_guild(bp.guild_id.as_deref(), bp.guild_name.as_deref()) {
                    our_count += 1;
                    our_deaths += bp.deaths.unwrap_or(0);
                }
            }
        }
        let cached_cell = match db.battles.get(&b.id) {
            Some(sb) if !sb.price_snapshot.is_empty() => {
                let ts_str = sb
                    .price_snapshot_at
                    .map(short_unix_time)
                    .unwrap_or_else(|| "已锁定".to_string());
                Cell::new(format!("已存(快照 {ts_str})")).fg(Color::Green)
            }
            Some(_) => Cell::new("已存(无快照)").fg(Color::Yellow),
            None => Cell::new("-").fg(Color::DarkGrey),
        };
        let death_cell = if our_deaths > 0 {
            cell_r(our_deaths.to_string()).fg(Color::Red)
        } else {
            cell_r("0")
        };

        table.add_row(vec![
            cell_r((idx + 1).to_string()),
            Cell::new(b.id.to_string()),
            Cell::new(short_time(&b.start_time)),
            cell_r(b.total_kills.to_string()),
            cell_r(our_count.to_string()),
            death_cell,
            cached_cell,
        ]);
    }
    println!("{table}");
}

/// 询问是否覆盖已有的历史价格快照（防止误改历史账本）
fn confirm_overwrite_snapshot(
    battle_id: u64,
    snap_len: usize,
    snap_at: Option<u64>,
    overwrite_all: &mut bool,
) -> bool {
    if *overwrite_all {
        return true;
    }
    let time_hint = snap_at
        .map(short_unix_time)
        .unwrap_or_else(|| "未知时间".to_string());
    println!(
        "\n⚠️  警告：Battle {battle_id} 已保存有历史价格快照（冻结于 {time_hint}，含 {snap_len} 条物品定价）。"
    );
    println!("    直接覆盖将会用当前最新市场价替换当时战损价格，可能影响历史对账！");
    print!("    输入 YES 确认覆盖本场 / YESALL 确认覆盖本次所有场次 / 直接回车保留原快照: ");
    let _ = io::stdout().flush();

    let mut ans = String::new();
    if io::stdin().read_line(&mut ans).is_err() {
        return false;
    }
    let trimmed = ans.trim();
    if trimmed == "YESALL" {
        *overwrite_all = true;
        true
    } else {
        trimmed == "YES"
    }
}

/// 更新单场战斗的中文名、物价，并生成/更新该场专属价格快照
async fn update_single_battle_prices(
    client: &reqwest::Client,
    stored: &mut StoredBattle,
    db: &mut LocalDb,
    live_prices: &mut HashMap<(String, u8), u64>,
    recent_trades: &mut HashMap<(String, u8), u64>,
) -> Result<(usize, usize, usize), Box<dyn Error>> {
    let mut needed_pairs: Vec<(String, u8)> = Vec::new();
    let mut all_item_ids: Vec<String> = Vec::new();

    for ev in &stored.events {
        if let Some(eq) = &ev.victim.equipment {
            for (_, _, maybe_item) in eq.all_slots() {
                if let Some(item) = maybe_item {
                    if let Some(id) = &item.item_type {
                        if !is_untradable_item(id) {
                            all_item_ids.push(id.clone());
                            needed_pairs.push((id.clone(), item.quality.unwrap_or(1).max(1)));
                        }
                    }
                }
            }
        }
        if let Some(inv) = &ev.victim.inventory {
            for item in inv.iter().flatten() {
                if let Some(id) = &item.item_type {
                    if !is_untradable_item(id) {
                        all_item_ids.push(id.clone());
                        needed_pairs.push((id.clone(), item.quality.unwrap_or(1).max(1)));
                    }
                }
            }
        }
        if let Some(eq) = &ev.killer.equipment {
            if let Some(id) = eq.main_hand.as_ref().and_then(|i| i.item_type.as_ref()) {
                all_item_ids.push(id.clone());
            }
        }
        if let Some(parts) = &ev.participants {
            for part in parts {
                if let Some(eq) = &part.equipment {
                    if let Some(id) = eq.main_hand.as_ref().and_then(|i| i.item_type.as_ref()) {
                        all_item_ids.push(id.clone());
                    }
                }
            }
        }
    }

    let names_added = ensure_zh_names(client, &all_item_ids, db).await;
    let exact_ids: Vec<String> = needed_pairs.iter().map(|(id, _)| id.clone()).collect();
    let (mut live_updated, mut trade_added) =
        fetch_prices_and_recent_trades(client, &exact_ids, db, live_prices, recent_trades).await?;

    let mut sibling_ids_to_fetch = Vec::new();
    for (id, q) in &needed_pairs {
        if lookup_exact_or_lower_quality(live_prices, recent_trades, None, db, id, *q, true)
            .is_none()
        {
            let (_, sibs) = equivalent_tier_siblings(id);
            sibling_ids_to_fetch.extend(sibs);
        }
    }
    if !sibling_ids_to_fetch.is_empty() {
        let (sib_live, sib_trade) = fetch_prices_and_recent_trades(
            client,
            &sibling_ids_to_fetch,
            db,
            live_prices,
            recent_trades,
        )
        .await?;
        live_updated += sib_live;
        trade_added += sib_trade;
    }

    let snap = build_battle_snapshot(&stored.events, live_prices, recent_trades, db);
    stored.price_snapshot = snap;
    stored.price_snapshot_at = Some(now_unix());
    db.battles.insert(stored.summary.id, stored.clone());
    save_db(db);

    Ok((names_added, live_updated, trade_added))
}

/// 评估并打印战报的核心渲染函数（可用于单场或合并后的一组战斗）
fn render_battle_report(
    heading: &str,
    stored_battles: &[StoredBattle],
    merged_events: &[KillEvent],
    event_snapshots: &[Option<&HashMap<String, PriceRecord>>],
    live_prices: &HashMap<(String, u8), u64>,
    recent_trades: &HashMap<(String, u8), u64>,
    db: &LocalDb,
    print_full_death_list: bool,
    show_our_details: bool,
    show_all_details: bool,
) {
    let mut grand_cross_cost = 0u64;
    let mut grand_full_cost = 0u64;
    let mut grand_inv_cost = 0u64;
    let mut grand_cross_missing = 0usize;
    let mut grand_total_missing = 0usize;
    let mut grand_inv_missing = 0usize;
    let mut grand_cross_approx = false;
    let mut grand_total_approx = false;
    let mut grand_inv_approx = false;

    let mut alliance_losses: HashMap<String, AllianceLossAgg> = HashMap::new();
    let mut guild_losses: HashMap<(String, String), GuildLossAgg> = HashMap::new();
    let mut evaluated_events = Vec::with_capacity(merged_events.len());
    let mut combatants: HashMap<String, CombatantStats> = HashMap::new();

    let upsert_combatant = |map: &mut HashMap<String, CombatantStats>,
                            pid: Option<&str>,
                            name: &str,
                            gid: Option<&str>,
                            gname: Option<&str>,
                            aid: Option<&str>,
                            aname: Option<&str>,
                            eq: Option<&Equipment>,
                            ip: Option<f64>,
                            ev_id: Option<u64>,
                            db: &LocalDb| {
        let entry = map
            .entry(name.to_string())
            .or_insert_with(|| CombatantStats {
                player_id: pid.map(str::to_string),
                name: name.to_string(),
                guild_id: gid.map(str::to_string),
                guild_name: gname.map(str::to_string),
                alliance_id: aid.map(str::to_string),
                alliance_name: aname.map(str::to_string),
                relation: Relation::Hostile,
                alliance_key: String::new(),
                alliance_display: String::new(),
                kills: 0,
                assists: 0,
                deaths: 0,
                heal_events: 0,
                total_damage: 0.0,
                total_healing: 0.0,
                max_single_healing: 0.0,
                main_hand: None,
                avg_ip: None,
                sample_event_id: ev_id,
            });
        if entry.player_id.is_none() {
            entry.player_id = non_empty(pid).map(str::to_string);
        }
        if entry.guild_id.is_none() {
            entry.guild_id = non_empty(gid).map(str::to_string);
        }
        if entry.guild_name.is_none() {
            entry.guild_name = non_empty(gname).map(str::to_string);
        }
        if entry.alliance_id.is_none() {
            entry.alliance_id = non_empty(aid).map(str::to_string);
        }
        if entry.alliance_name.is_none() {
            entry.alliance_name = non_empty(aname).map(str::to_string);
        }
        if entry.main_hand.is_none() {
            if let Some(mh) = eq
                .and_then(|e| e.main_hand.as_ref())
                .and_then(|i| i.item_type.as_ref())
            {
                entry.main_hand = Some(mh.clone());
            }
        }
        if entry.avg_ip.is_none() {
            if let Some(v) = ip.filter(|&x| x > 0.0) {
                entry.avg_ip = Some(v);
            }
        }
        if entry.sample_event_id.is_none() {
            entry.sample_event_id = ev_id;
        }
        let rel = classify_relation(
            entry.guild_id.as_deref(),
            entry.guild_name.as_deref(),
            entry.alliance_id.as_deref(),
            entry.alliance_name.as_deref(),
            db,
        );
        let (ak, ad) = alliance_bucket(entry.alliance_name.as_deref(), rel);
        entry.relation = rel;
        entry.alliance_key = ak;
        entry.alliance_display = ad;
    };

    for sb in stored_battles {
        if let Some(players) = &sb.summary.players {
            for (pid, bp) in players {
                upsert_combatant(
                    &mut combatants,
                    Some(pid.as_str()),
                    &bp.name,
                    bp.guild_id.as_deref(),
                    bp.guild_name.as_deref(),
                    bp.alliance_id.as_deref(),
                    bp.alliance_name.as_deref(),
                    None,
                    None,
                    None,
                    db,
                );
                if let Some(c) = combatants.get_mut(&bp.name) {
                    c.kills += bp.kills.unwrap_or(0);
                    c.deaths += bp.deaths.unwrap_or(0);
                }
            }
        }
    }

    let mut combatant_ev_kills: HashMap<String, u32> = HashMap::new();
    let mut combatant_ev_deaths: HashMap<String, u32> = HashMap::new();

    for (idx, (ev, snap_opt)) in merged_events.iter().zip(event_snapshots.iter()).enumerate() {
        let gear_eval = evaluate_equipment(
            ev.victim.equipment.as_ref(),
            live_prices,
            recent_trades,
            *snap_opt,
            db,
        );
        let inv_eval = evaluate_inventory(
            ev.victim.inventory.as_ref(),
            live_prices,
            recent_trades,
            *snap_opt,
            db,
        );

        grand_cross_cost += gear_eval.cross_cost;
        grand_full_cost += gear_eval.full_cost;
        grand_inv_cost += inv_eval.inv_cost;
        grand_cross_missing += gear_eval.cross_missing;
        grand_total_missing += gear_eval.total_missing;
        grand_inv_missing += inv_eval.inv_missing;
        grand_cross_approx |= gear_eval.cross_approx;
        grand_total_approx |= gear_eval.total_approx;
        grand_inv_approx |= inv_eval.inv_approx;

        upsert_combatant(
            &mut combatants,
            ev.killer.id.as_deref(),
            &ev.killer.name,
            ev.killer.guild_id.as_deref(),
            ev.killer.guild_name.as_deref(),
            ev.killer.alliance_id.as_deref(),
            ev.killer.alliance_name.as_deref(),
            ev.killer.equipment.as_ref(),
            ev.killer.average_item_power,
            Some(ev.event_id),
            db,
        );
        *combatant_ev_kills
            .entry(ev.killer.name.clone())
            .or_default() += 1;

        upsert_combatant(
            &mut combatants,
            ev.victim.id.as_deref(),
            &ev.victim.name,
            ev.victim.guild_id.as_deref(),
            ev.victim.guild_name.as_deref(),
            ev.victim.alliance_id.as_deref(),
            ev.victim.alliance_name.as_deref(),
            ev.victim.equipment.as_ref(),
            ev.victim.average_item_power,
            Some(ev.event_id),
            db,
        );
        *combatant_ev_deaths
            .entry(ev.victim.name.clone())
            .or_default() += 1;

        if let Some(parts) = &ev.participants {
            for p in parts {
                upsert_combatant(
                    &mut combatants,
                    p.id.as_deref(),
                    &p.name,
                    p.guild_id.as_deref(),
                    p.guild_name.as_deref(),
                    p.alliance_id.as_deref(),
                    p.alliance_name.as_deref(),
                    p.equipment.as_ref(),
                    p.average_item_power,
                    Some(ev.event_id),
                    db,
                );
                if let Some(c) = combatants.get_mut(&p.name) {
                    if p.name != ev.killer.name {
                        c.assists += 1;
                    }
                    let dmg = p.damage_done.unwrap_or(0.0);
                    let heal = p.support_healing_done.unwrap_or(0.0);
                    c.total_damage += dmg;
                    c.total_healing += heal;
                    if heal > 0.0 {
                        c.heal_events += 1;
                        if heal > c.max_single_healing {
                            c.max_single_healing = heal;
                        }
                    }
                }
            }
        }
        if let Some(members) = &ev.group_members {
            for gm in members {
                upsert_combatant(
                    &mut combatants,
                    gm.id.as_deref(),
                    &gm.name,
                    gm.guild_id.as_deref(),
                    gm.guild_name.as_deref(),
                    gm.alliance_id.as_deref(),
                    gm.alliance_name.as_deref(),
                    gm.equipment.as_ref(),
                    gm.average_item_power,
                    Some(ev.event_id),
                    db,
                );
            }
        }

        let rel = classify_relation(
            ev.victim.guild_id.as_deref(),
            ev.victim.guild_name.as_deref(),
            ev.victim.alliance_id.as_deref(),
            ev.victim.alliance_name.as_deref(),
            db,
        );
        let (a_key, a_disp) = alliance_bucket(ev.victim.alliance_name.as_deref(), rel);
        let g_label = non_empty(ev.victim.guild_name.as_deref())
            .unwrap_or("[无公会]")
            .to_string();

        let a_agg = alliance_losses
            .entry(a_key.clone())
            .or_insert_with(|| AllianceLossAgg {
                alliance_display: a_disp.clone(),
                relation: rel,
                guilds: HashSet::new(),
                participants: HashSet::new(),
                deaths: 0,
                cross_cost: 0,
                full_cost: 0,
                inv_cost: 0,
                cross_missing: 0,
                total_missing: 0,
                inv_missing: 0,
                cross_approx: false,
                total_approx: false,
                inv_approx: false,
            });
        a_agg.guilds.insert(g_label.clone());
        a_agg.deaths += 1;
        a_agg.cross_cost += gear_eval.cross_cost;
        a_agg.full_cost += gear_eval.full_cost;
        a_agg.inv_cost += inv_eval.inv_cost;
        a_agg.cross_missing += gear_eval.cross_missing;
        a_agg.total_missing += gear_eval.total_missing;
        a_agg.inv_missing += inv_eval.inv_missing;
        a_agg.cross_approx |= gear_eval.cross_approx;
        a_agg.total_approx |= gear_eval.total_approx;
        a_agg.inv_approx |= inv_eval.inv_approx;

        let g_agg = guild_losses
            .entry((a_key.clone(), g_label.clone()))
            .or_insert_with(|| GuildLossAgg {
                alliance_key: a_key.clone(),
                alliance_display: a_disp.clone(),
                guild_name: g_label.clone(),
                relation: rel,
                participants: HashSet::new(),
                deaths: 0,
                cross_cost: 0,
                full_cost: 0,
                inv_cost: 0,
                cross_missing: 0,
                total_missing: 0,
                inv_missing: 0,
                cross_approx: false,
                total_approx: false,
                inv_approx: false,
            });
        g_agg.deaths += 1;
        g_agg.cross_cost += gear_eval.cross_cost;
        g_agg.full_cost += gear_eval.full_cost;
        g_agg.inv_cost += inv_eval.inv_cost;
        g_agg.cross_missing += gear_eval.cross_missing;
        g_agg.total_missing += gear_eval.total_missing;
        g_agg.inv_missing += inv_eval.inv_missing;
        g_agg.cross_approx |= gear_eval.cross_approx;
        g_agg.total_approx |= gear_eval.total_approx;
        g_agg.inv_approx |= inv_eval.inv_approx;

        evaluated_events.push(EvaluatedEvent {
            idx_original: idx,
            relation: rel,
            alliance_key: a_key,
            alliance_display: a_disp,
            guild_label: g_label,
            gear_eval,
            inv_eval,
        });
    }

    for (name, c) in &mut combatants {
        if let Some(&k) = combatant_ev_kills.get(name) {
            if k > c.kills {
                c.kills = k;
            }
        }
        if let Some(&d) = combatant_ev_deaths.get(name) {
            if d > c.deaths {
                c.deaths = d;
            }
        }
        let g_label = non_empty(c.guild_name.as_deref())
            .unwrap_or("[无公会]")
            .to_string();
        let a_agg = alliance_losses
            .entry(c.alliance_key.clone())
            .or_insert_with(|| AllianceLossAgg {
                alliance_display: c.alliance_display.clone(),
                relation: c.relation,
                guilds: HashSet::new(),
                participants: HashSet::new(),
                deaths: 0,
                cross_cost: 0,
                full_cost: 0,
                inv_cost: 0,
                cross_missing: 0,
                total_missing: 0,
                inv_missing: 0,
                cross_approx: false,
                total_approx: false,
                inv_approx: false,
            });
        a_agg.guilds.insert(g_label.clone());
        a_agg.participants.insert(c.name.clone());

        if let Some(g_agg) = guild_losses.get_mut(&(c.alliance_key.clone(), g_label)) {
            g_agg.participants.insert(c.name.clone());
        }
    }

    let alliance_rank_val: HashMap<String, u64> = alliance_losses
        .iter()
        .map(|(k, v)| (k.clone(), v.full_cost + v.inv_cost))
        .collect();

    evaluated_events.sort_by(|a, b| {
        let a_tot = alliance_rank_val.get(&a.alliance_key).copied().unwrap_or(0);
        let b_tot = alliance_rank_val.get(&b.alliance_key).copied().unwrap_or(0);
        b_tot
            .cmp(&a_tot)
            .then_with(|| a.alliance_key.cmp(&b.alliance_key))
            .then_with(|| a.guild_label.cmp(&b.guild_label))
            .then_with(|| {
                merged_events[a.idx_original]
                    .time_stamp
                    .cmp(&merged_events[b.idx_original].time_stamp)
            })
    });

    let alive_winners_count = combatants.values().filter(|c| c.deaths == 0).count();

    println!(
        "\n{heading}（参战 {} 人 | 死亡 {} 次 | 0死存活 {} 人）   整场十字架: {}   整场全套: {}   整场包内: {}",
        combatants.len(),
        merged_events.len(),
        alive_winners_count,
        format_cost_text(grand_cross_cost, grand_cross_missing, grand_cross_approx),
        format_cost_text(grand_full_cost, grand_total_missing, grand_total_approx),
        format_cost_text(grand_inv_cost, grand_inv_missing, grand_inv_approx)
    );

    if print_full_death_list {
        println!();
        let mut deaths_table = new_clean_table();
        deaths_table.set_header(vec![
            hdr("#", true),
            hdr("时间", false),
            hdr("阵营", false),
            hdr("联盟", false),
            hdr("参战 / 死者 (可点)", false),
            hdr("击杀者 / 战绩 (可点)", false),
            hdr("十字架", true),
            hdr("全套总价", true),
            hdr("包内价值", true),
            hdr("死者主手", false),
            hdr("助攻", true),
            hdr("最高治疗 / 治疗者", false),
        ]);

        for (row_idx, item) in evaluated_events.iter().enumerate() {
            let ev = &merged_events[item.idx_original];
            let killer_tag = format_player_tag(&ev.killer.name, ev.killer.guild_name.as_deref());
            let killer_linked =
                player_link(ev.killer.id.as_deref(), Some(ev.event_id), &killer_tag);
            let victim_raw = format_player_tag(&ev.victim.name, ev.victim.guild_name.as_deref());
            let victim_linked =
                player_link(ev.victim.id.as_deref(), Some(ev.event_id), &victim_raw);

            let mh_cell = match ev
                .victim
                .equipment
                .as_ref()
                .and_then(|e| e.main_hand.as_ref())
                .and_then(|i| i.item_type.as_deref())
            {
                Some(mh) => item_cell(mh, &db.item_names_zh),
                None => Cell::new("-"),
            };

            let mut assist_count = 0usize;
            let mut top_healer: Option<(&str, Option<&str>, f64)> = None;
            if let Some(parts) = &ev.participants {
                for p in parts {
                    if p.name != ev.killer.name {
                        assist_count += 1;
                    }
                    let heal = p.support_healing_done.unwrap_or(0.0);
                    if heal > 0.0 && top_healer.map_or(true, |(_, _, max_h)| heal > max_h) {
                        let pid_fallback = p.id.as_deref().or_else(|| {
                            combatants.get(&p.name).and_then(|c| c.player_id.as_deref())
                        });
                        top_healer = Some((p.name.as_str(), pid_fallback, heal));
                    }
                }
            }

            let heal_cell = match top_healer {
                Some((name, pid, val)) => {
                    let txt = format!("{name} ({})", fmt_num(val.round() as u64));
                    Cell::new(player_link(pid, Some(ev.event_id), &txt)).fg(Color::Green)
                }
                None => Cell::new("-").fg(Color::DarkGrey),
            };

            deaths_table.add_row(vec![
                cell_r((row_idx + 1).to_string()),
                Cell::new(short_hms(&ev.time_stamp)),
                relation_cell(item.relation),
                apply_relation_color(Cell::new(&item.alliance_display), item.relation),
                apply_relation_color(Cell::new(victim_linked), item.relation),
                Cell::new(killer_linked),
                format_cost_cell(
                    item.gear_eval.cross_cost,
                    item.gear_eval.cross_missing,
                    item.gear_eval.cross_approx,
                ),
                format_cost_cell(
                    item.gear_eval.full_cost,
                    item.gear_eval.total_missing,
                    item.gear_eval.total_approx,
                ),
                format_cost_cell(
                    item.inv_eval.inv_cost,
                    item.inv_eval.inv_missing,
                    item.inv_eval.inv_approx,
                ),
                mh_cell,
                cell_r(assist_count.to_string()),
                heal_cell,
            ]);
        }

        // 所有 0 死亡的胜利者放在列表最后
        let mut alive_winners: Vec<&CombatantStats> =
            combatants.values().filter(|c| c.deaths == 0).collect();
        alive_winners.sort_by(|a, b| {
            a.alliance_key
                .cmp(&b.alliance_key)
                .then_with(|| a.guild_name.cmp(&b.guild_name))
                .then_with(|| (b.kills + b.assists).cmp(&(a.kills + a.assists)))
                .then_with(|| b.total_healing.total_cmp(&a.total_healing))
                .then_with(|| a.name.cmp(&b.name))
        });

        let base_idx = evaluated_events.len();
        for (offset_idx, c) in alive_winners.iter().enumerate() {
            let p_raw = format_player_tag(&c.name, c.guild_name.as_deref());
            let p_linked = player_link(c.player_id.as_deref(), c.sample_event_id, &p_raw);
            let mh_cell = match c.main_hand.as_deref() {
                Some(mh) => item_cell(mh, &db.item_names_zh),
                None => Cell::new("-"),
            };
            let record_str = format!(
                "杀{} 助{} 伤{}",
                c.kills,
                c.assists,
                fmt_num(c.total_damage.round() as u64)
            );
            let heal_val = c.total_healing.round() as u64;
            let heal_cell = if heal_val > 0 {
                cell_r(fmt_num(heal_val)).fg(Color::Green)
            } else {
                Cell::new("-").fg(Color::DarkGrey)
            };

            deaths_table.add_row(vec![
                cell_r((base_idx + offset_idx + 1).to_string()),
                Cell::new("0死(存活)").fg(Color::Green),
                relation_cell(c.relation),
                apply_relation_color(Cell::new(&c.alliance_display), c.relation),
                apply_relation_color(Cell::new(p_linked), c.relation),
                Cell::new(record_str).fg(Color::DarkGrey),
                cell_r("0"),
                cell_r("0"),
                cell_r("0"),
                mh_cell,
                cell_r(c.assists.to_string()),
                heal_cell,
            ]);
        }

        println!("{deaths_table}");
    }

    // 各联盟掉落总价
    let mut alliance_list: Vec<AllianceLossAgg> = alliance_losses.into_values().collect();
    alliance_list.sort_by(|a, b| {
        (b.full_cost + b.inv_cost)
            .cmp(&(a.full_cost + a.inv_cost))
            .then_with(|| b.deaths.cmp(&a.deaths))
    });

    println!("\n各联盟掉落总价格统计（共 {} 组）\n", alliance_list.len());
    let mut al_table = new_clean_table();
    al_table.set_header(vec![
        hdr("排名", true),
        hdr("阵营", false),
        hdr("联盟", false),
        hdr("公会数", true),
        hdr("参战", true),
        hdr("死亡", true),
        hdr("十字架总价", true),
        hdr("全套总价", true),
        hdr("包内总价", true),
        hdr("联盟总爆(套+包)", true),
    ]);
    for (idx, a) in alliance_list.iter().enumerate() {
        al_table.add_row(vec![
            cell_r((idx + 1).to_string()),
            relation_cell(a.relation),
            apply_relation_color(Cell::new(&a.alliance_display), a.relation),
            cell_r(a.guilds.len().to_string()),
            cell_r(a.participants.len().to_string()),
            cell_r(a.deaths.to_string()),
            format_cost_cell(a.cross_cost, a.cross_missing, a.cross_approx),
            format_cost_cell(a.full_cost, a.total_missing, a.total_approx),
            format_cost_cell(a.inv_cost, a.inv_missing, a.inv_approx),
            format_cost_cell(
                a.full_cost + a.inv_cost,
                a.total_missing + a.inv_missing,
                a.total_approx || a.inv_approx,
            ),
        ]);
    }
    println!("{al_table}");

    // 各公会掉落总价（同联盟聚拢）
    let mut guild_loss_list: Vec<GuildLossAgg> = guild_losses.into_values().collect();
    guild_loss_list.sort_by(|a, b| {
        let a_tot = alliance_rank_val.get(&a.alliance_key).copied().unwrap_or(0);
        let b_tot = alliance_rank_val.get(&b.alliance_key).copied().unwrap_or(0);
        b_tot
            .cmp(&a_tot)
            .then_with(|| a.alliance_key.cmp(&b.alliance_key))
            .then_with(|| (b.full_cost + b.inv_cost).cmp(&(a.full_cost + a.inv_cost)))
            .then_with(|| b.deaths.cmp(&a.deaths))
    });

    println!(
        "\n各公会掉落总价格统计（共 {} 个公会，同联盟聚拢排序）\n",
        guild_loss_list.len()
    );
    let mut gl_table = new_clean_table();
    gl_table.set_header(vec![
        hdr("排名", true),
        hdr("阵营", false),
        hdr("联盟", false),
        hdr("公会", false),
        hdr("参战", true),
        hdr("死亡", true),
        hdr("十字架总价", true),
        hdr("全套总价", true),
        hdr("包内总价", true),
        hdr("公会总爆(套+包)", true),
    ]);
    for (idx, g) in guild_loss_list.iter().enumerate() {
        gl_table.add_row(vec![
            cell_r((idx + 1).to_string()),
            relation_cell(g.relation),
            apply_relation_color(Cell::new(&g.alliance_display), g.relation),
            apply_relation_color(Cell::new(&g.guild_name), g.relation),
            cell_r(g.participants.len().to_string()),
            cell_r(g.deaths.to_string()),
            format_cost_cell(g.cross_cost, g.cross_missing, g.cross_approx),
            format_cost_cell(g.full_cost, g.total_missing, g.total_approx),
            format_cost_cell(g.inv_cost, g.inv_missing, g.inv_approx),
            format_cost_cell(
                g.full_cost + g.inv_cost,
                g.total_missing + g.inv_missing,
                g.total_approx || g.inv_approx,
            ),
        ]);
    }
    println!("{gl_table}");

    // 全场治疗排行榜 & 各联盟治疗实力综合对比
    let mut healers: Vec<&CombatantStats> = combatants
        .values()
        .filter(|c| c.total_healing > 0.0)
        .collect();
    healers.sort_by(|a, b| {
        b.total_healing
            .total_cmp(&a.total_healing)
            .then_with(|| b.max_single_healing.total_cmp(&a.max_single_healing))
    });

    if !healers.is_empty() {
        let mut alliance_heals: HashMap<String, AllianceHealAgg> = HashMap::new();
        for h in &healers {
            let entry = alliance_heals
                .entry(h.alliance_key.clone())
                .or_insert_with(|| AllianceHealAgg {
                    alliance_display: h.alliance_display.clone(),
                    relation: h.relation,
                    healer_count: 0,
                    healer_deaths: 0,
                    total_healing: 0.0,
                    top_healer_name: h.name.clone(),
                    top_healer_val: h.total_healing,
                });
            entry.healer_count += 1;
            if h.deaths > 0 {
                entry.healer_deaths += 1;
            }
            entry.total_healing += h.total_healing;
            if h.total_healing > entry.top_healer_val {
                entry.top_healer_val = h.total_healing;
                entry.top_healer_name = h.name.clone();
            }
        }

        let mut ah_list: Vec<AllianceHealAgg> = alliance_heals.into_values().collect();
        ah_list.sort_by(|a, b| b.total_healing.total_cmp(&a.total_healing));

        println!(
            "\n各联盟治疗实力综合对比（共 {} 组有治疗产出）\n",
            ah_list.len()
        );
        let mut ah_table = new_clean_table();
        ah_table.set_header(vec![
            hdr("排名", true),
            hdr("阵营", false),
            hdr("联盟", false),
            hdr("奶妈数", true),
            hdr("奶妈阵亡", true),
            hdr("联盟总治疗", true),
            hdr("人均治疗", true),
            hdr("最强奶妈 (总治疗)", false),
        ]);
        for (idx, ah) in ah_list.iter().enumerate() {
            let avg = if ah.healer_count > 0 {
                (ah.total_healing / ah.healer_count as f64).round() as u64
            } else {
                0
            };
            ah_table.add_row(vec![
                cell_r((idx + 1).to_string()),
                relation_cell(ah.relation),
                apply_relation_color(Cell::new(&ah.alliance_display), ah.relation),
                cell_r(ah.healer_count.to_string()),
                if ah.healer_deaths > 0 {
                    cell_r(ah.healer_deaths.to_string()).fg(Color::Red)
                } else {
                    cell_r("0")
                },
                cell_r(fmt_num(ah.total_healing.round() as u64)).fg(Color::Green),
                cell_r(fmt_num(avg)),
                Cell::new(format!(
                    "{} ({})",
                    ah.top_healer_name,
                    fmt_num(ah.top_healer_val.round() as u64)
                )),
            ]);
        }
        println!("{ah_table}");

        let show_limit = if show_all_details {
            healers.len()
        } else {
            25.min(healers.len())
        };
        println!(
            "\n全场治疗排行榜（前 {} / 共 {} 位奶妈，名字可点看配装）\n",
            show_limit,
            healers.len()
        );
        let mut h_table = new_clean_table();
        h_table.set_header(vec![
            hdr("排名", true),
            hdr("阵营", false),
            hdr("联盟", false),
            hdr("治疗者 (可点)", false),
            hdr("治疗武器", false),
            hdr("IP", true),
            hdr("死亡", true),
            hdr("有效参与", true),
            hdr("累计治疗量", true),
            hdr("单次最高", true),
        ]);
        for (idx, h) in healers.iter().take(show_limit).enumerate() {
            let p_raw = format_player_tag(&h.name, h.guild_name.as_deref());
            let p_linked = player_link(h.player_id.as_deref(), h.sample_event_id, &p_raw);
            let mh_cell = match h.main_hand.as_deref() {
                Some(mh) => item_cell(mh, &db.item_names_zh),
                None => Cell::new("-"),
            };
            let ip = h
                .avg_ip
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "-".to_string());
            h_table.add_row(vec![
                cell_r((idx + 1).to_string()),
                relation_cell(h.relation),
                apply_relation_color(Cell::new(&h.alliance_display), h.relation),
                apply_relation_color(Cell::new(p_linked), h.relation),
                mh_cell,
                cell_r(ip),
                if h.deaths > 0 {
                    cell_r(h.deaths.to_string()).fg(Color::Red)
                } else {
                    cell_r("0")
                },
                cell_r(h.heal_events.to_string()),
                cell_r(fmt_num(h.total_healing.round() as u64)).fg(Color::Green),
                cell_r(fmt_num(h.max_single_healing.round() as u64)),
            ]);
        }
        println!("{h_table}");
    }

    // 我方公会参战名单
    let mut guild_roster: HashMap<String, GuildMemberStats> = HashMap::new();
    for sb in stored_battles {
        let mut attended_this = HashSet::new();
        if let Some(players) = &sb.summary.players {
            for (pid, bp) in players {
                if is_my_guild(bp.guild_id.as_deref(), bp.guild_name.as_deref()) {
                    attended_this.insert(bp.name.clone());
                    let entry =
                        guild_roster
                            .entry(bp.name.clone())
                            .or_insert_with(|| GuildMemberStats {
                                player_id: Some(pid.clone()),
                                name: bp.name.clone(),
                                ..Default::default()
                            });
                    entry.kills += bp.kills.unwrap_or(0);
                    entry.deaths += bp.deaths.unwrap_or(0);
                }
            }
        }
        for name in attended_this {
            if let Some(e) = guild_roster.get_mut(&name) {
                e.battles_attended += 1;
            }
        }
    }

    let mut event_kills_count: HashMap<String, u32> = HashMap::new();
    for item in &evaluated_events {
        let ev = &merged_events[item.idx_original];
        if is_my_guild(
            ev.killer.guild_id.as_deref(),
            ev.killer.guild_name.as_deref(),
        ) {
            *event_kills_count.entry(ev.killer.name.clone()).or_default() += 1;
            let entry = guild_roster
                .entry(ev.killer.name.clone())
                .or_insert_with(|| GuildMemberStats {
                    player_id: ev.killer.id.clone(),
                    name: ev.killer.name.clone(),
                    ..Default::default()
                });
            update_weapon_ip(
                entry,
                ev.killer.equipment.as_ref(),
                ev.killer.average_item_power,
            );
        }
        if is_my_guild(
            ev.victim.guild_id.as_deref(),
            ev.victim.guild_name.as_deref(),
        ) {
            let entry = guild_roster
                .entry(ev.victim.name.clone())
                .or_insert_with(|| GuildMemberStats {
                    player_id: ev.victim.id.clone(),
                    name: ev.victim.name.clone(),
                    ..Default::default()
                });
            update_weapon_ip(
                entry,
                ev.victim.equipment.as_ref(),
                ev.victim.average_item_power,
            );
            entry.cross_total += item.gear_eval.cross_cost;
            entry.full_total += item.gear_eval.full_cost;
            entry.inv_total += item.inv_eval.inv_cost;
            entry.cross_missing += item.gear_eval.cross_missing;
            entry.total_missing += item.gear_eval.total_missing;
            entry.inv_missing += item.inv_eval.inv_missing;
            entry.cross_approx |= item.gear_eval.cross_approx;
            entry.total_approx |= item.gear_eval.total_approx;
            entry.inv_approx |= item.inv_eval.inv_approx;
            entry.death_records.push(DeathRecord {
                event_id: ev.event_id,
                timestamp: ev.time_stamp.clone(),
                killer_display: format_player_tag(&ev.killer.name, ev.killer.guild_name.as_deref()),
                cross_cost: item.gear_eval.cross_cost,
                full_cost: item.gear_eval.full_cost,
                inv_cost: item.inv_eval.inv_cost,
                cross_missing: item.gear_eval.cross_missing,
                total_missing: item.gear_eval.total_missing,
                inv_missing: item.inv_eval.inv_missing,
                cross_approx: item.gear_eval.cross_approx,
                total_approx: item.gear_eval.total_approx,
                inv_approx: item.inv_eval.inv_approx,
                gears: item.gear_eval.gears.clone(),
                inv_items: item.inv_eval.items.clone(),
            });
        }
        if let Some(parts) = &ev.participants {
            for p in parts {
                if is_my_guild(p.guild_id.as_deref(), p.guild_name.as_deref()) {
                    let entry =
                        guild_roster
                            .entry(p.name.clone())
                            .or_insert_with(|| GuildMemberStats {
                                player_id: p.id.clone(),
                                name: p.name.clone(),
                                ..Default::default()
                            });
                    if p.name != ev.killer.name {
                        entry.assists += 1;
                    }
                    entry.total_damage += p.damage_done.unwrap_or(0.0);
                    entry.total_healing += p.support_healing_done.unwrap_or(0.0);
                    update_weapon_ip(entry, p.equipment.as_ref(), p.average_item_power);
                }
            }
        }
        if let Some(members) = &ev.group_members {
            for gm in members {
                if is_my_guild(gm.guild_id.as_deref(), gm.guild_name.as_deref()) {
                    let entry =
                        guild_roster
                            .entry(gm.name.clone())
                            .or_insert_with(|| GuildMemberStats {
                                player_id: gm.id.clone(),
                                name: gm.name.clone(),
                                ..Default::default()
                            });
                    update_weapon_ip(entry, gm.equipment.as_ref(), gm.average_item_power);
                }
            }
        }
    }

    for (name, stats) in &mut guild_roster {
        if stats.player_id.is_none() {
            stats.player_id = combatants.get(name).and_then(|c| c.player_id.clone());
        }
        let ev_deaths = stats.death_records.len() as u32;
        if ev_deaths > stats.deaths {
            stats.deaths = ev_deaths;
        }
        if let Some(&k) = event_kills_count.get(name) {
            if k > stats.kills {
                stats.kills = k;
            }
        }
    }

    let our_cross_sum: u64 = guild_roster.values().map(|p| p.cross_total).sum();
    let our_full_sum: u64 = guild_roster.values().map(|p| p.full_total).sum();
    let our_inv_sum: u64 = guild_roster.values().map(|p| p.inv_total).sum();
    let our_cross_miss: usize = guild_roster.values().map(|p| p.cross_missing).sum();
    let our_full_miss: usize = guild_roster.values().map(|p| p.total_missing).sum();
    let our_inv_miss: usize = guild_roster.values().map(|p| p.inv_missing).sum();
    let our_cross_approx = guild_roster.values().any(|p| p.cross_approx);
    let our_full_approx = guild_roster.values().any(|p| p.total_approx);
    let our_inv_approx = guild_roster.values().any(|p| p.inv_approx);

    let mut ranked: Vec<GuildMemberStats> = guild_roster.into_values().collect();
    ranked.sort_by(|a, b| {
        b.deaths
            .cmp(&a.deaths)
            .then_with(|| b.full_total.cmp(&a.full_total))
            .then_with(|| b.cross_total.cmp(&a.cross_total))
            .then_with(|| {
                (b.total_healing + b.total_damage).total_cmp(&(a.total_healing + a.total_damage))
            })
            .then_with(|| a.name.cmp(&b.name))
    });

    println!(
        "\n公会「{}」参战名单 & 死亡情况排名（共 {} 人）   我方十字架: {}   我方全套: {}   我方包内: {}\n",
        DEFAULT_GUILD_NAME,
        ranked.len(),
        format_cost_text(our_cross_sum, our_cross_miss, our_cross_approx),
        format_cost_text(our_full_sum, our_full_miss, our_full_approx),
        format_cost_text(our_inv_sum, our_inv_miss, our_inv_approx)
    );

    if !ranked.is_empty() {
        let mut guild_table = new_clean_table();
        let mut header_cells = vec![hdr("排名", true), hdr("玩家", false)];
        if stored_battles.len() > 1 {
            header_cells.push(hdr("出勤", true));
        }
        header_cells.extend([
            hdr("死亡", true),
            hdr("十字架损失", true),
            hdr("全套总损失", true),
            hdr("包内损失", true),
            hdr("击杀", true),
            hdr("助攻", true),
            hdr("总治疗量", true),
            hdr("总伤害量", true),
            hdr("主手装备", false),
            hdr("IP", true),
        ]);
        guild_table.set_header(header_cells);

        for (idx, p) in ranked.iter().enumerate() {
            let mh_cell = match p.main_hand.as_deref() {
                Some(mh) => item_cell(mh, &db.item_names_zh),
                None => Cell::new("-"),
            };
            let ip = p
                .avg_ip
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "-".to_string());

            let death_cell = if p.deaths > 0 {
                cell_r(p.deaths.to_string()).fg(Color::Red)
            } else {
                cell_r("0")
            };

            let heal_val = p.total_healing.round() as u64;
            let heal_cell = if heal_val > 0 {
                cell_r(fmt_num(heal_val)).fg(Color::Green)
            } else {
                cell_r("0")
            };

            let linked_name = player_link(
                p.player_id.as_deref(),
                p.death_records.first().map(|d| d.event_id),
                &p.name,
            );
            let player_cell = Cell::new(linked_name).add_attribute(Attribute::Bold);

            let mut row = vec![cell_r((idx + 1).to_string()), player_cell];
            if stored_battles.len() > 1 {
                row.push(cell_r(p.battles_attended.to_string()));
            }
            row.extend([
                death_cell,
                format_cost_cell(p.cross_total, p.cross_missing, p.cross_approx),
                format_cost_cell(p.full_total, p.total_missing, p.total_approx),
                format_cost_cell(p.inv_total, p.inv_missing, p.inv_approx),
                cell_r(p.kills.to_string()),
                cell_r(p.assists.to_string()),
                heal_cell,
                cell_r(fmt_num(p.total_damage.round() as u64)),
                mh_cell,
                cell_r(ip),
            ]);
            guild_table.add_row(row);
        }
        println!("{guild_table}");
    }

    if show_our_details || show_all_details {
        let any_deaths = ranked.iter().any(|p| !p.death_records.is_empty());
        if any_deaths {
            println!("\n公会阵亡人员装备 & 背包明细（-d 模式）");
            for p in &ranked {
                if p.death_records.is_empty() {
                    continue;
                }
                println!(
                    "\n  {}   死亡 {} 次   累计十字架: {}   累计全套: {}   累计包内: {}",
                    p.name,
                    p.deaths,
                    format_cost_text(p.cross_total, p.cross_missing, p.cross_approx),
                    format_cost_text(p.full_total, p.total_missing, p.total_approx),
                    format_cost_text(p.inv_total, p.inv_missing, p.inv_approx)
                );
                for (i, rec) in p.death_records.iter().enumerate() {
                    let ev_link =
                        osc8_link(&killboard_url(rec.event_id), &rec.event_id.to_string());
                    println!(
                        "  [第 {} 次]  死于: {}  ({})\n             官网直达: {}   十字架: {}   全套: {}   包内: {}\n",
                        i + 1,
                        rec.killer_display,
                        short_time(&rec.timestamp),
                        ev_link,
                        format_cost_text(rec.cross_cost, rec.cross_missing, rec.cross_approx),
                        format_cost_text(rec.full_cost, rec.total_missing, rec.total_approx),
                        format_cost_text(rec.inv_cost, rec.inv_missing, rec.inv_approx)
                    );
                    print_gear_table(Some("身上穿戴："), &rec.gears, &db.item_names_zh);
                    if !rec.inv_items.is_empty() {
                        println!();
                        print_gear_table(
                            Some(&format!(
                                "背包内物品（共 {} 类，总计 {}）：",
                                rec.inv_items.len(),
                                format_cost_text(rec.inv_cost, rec.inv_missing, rec.inv_approx)
                            )),
                            &rec.inv_items,
                            &db.item_names_zh,
                        );
                    }
                }
            }
        }
    }

    if show_all_details {
        println!("\n整场战斗逐次死亡装备 & 背包明细（-da 模式，按联盟归组）");
        for (row_idx, item) in evaluated_events.iter().enumerate() {
            let ev = &merged_events[item.idx_original];
            let victim_linked = osc8_link(
                &killboard_url(ev.event_id),
                &format_player_tag(&ev.victim.name, ev.victim.guild_name.as_deref()),
            );

            println!(
                "\n  #{:<2}  [{}] {}  ←击杀: {}",
                row_idx + 1,
                item.alliance_display,
                victim_linked,
                format_player_tag(&ev.killer.name, ev.killer.guild_name.as_deref())
            );
            println!(
                "       时间: {}   十字架: {}   全套: {}   包内: {}\n",
                short_time(&ev.time_stamp),
                format_cost_text(
                    item.gear_eval.cross_cost,
                    item.gear_eval.cross_missing,
                    item.gear_eval.cross_approx
                ),
                format_cost_text(
                    item.gear_eval.full_cost,
                    item.gear_eval.total_missing,
                    item.gear_eval.total_approx
                ),
                format_cost_text(
                    item.inv_eval.inv_cost,
                    item.inv_eval.inv_missing,
                    item.inv_eval.inv_approx
                )
            );
            print_gear_table(None, &item.gear_eval.gears, &db.item_names_zh);
            if !item.inv_eval.items.is_empty() {
                println!();
                print_gear_table(
                    Some(&format!(
                        "背包内物品（共 {} 类，总计 {}）：",
                        item.inv_eval.items.len(),
                        format_cost_text(
                            item.inv_eval.inv_cost,
                            item.inv_eval.inv_missing,
                            item.inv_eval.inv_approx
                        )
                    )),
                    &item.inv_eval.items,
                    &db.item_names_zh,
                );
            }
        }
    }
}

pub async fn analyze_battles(
    client: &reqwest::Client,
    battle_ids: &[u64],
    db: &mut LocalDb,
    show_our_details: bool,
    show_all_details: bool,
    force_refresh: bool,
    update_prices: bool,
) -> Result<(), Box<dyn Error>> {
    if battle_ids.is_empty() {
        return Ok(());
    }

    if update_prices || (db.my_alliance_id.is_none() && db.manual_alliance.is_none()) {
        refresh_my_alliance(client, db).await;
    }

    let is_multi = battle_ids.len() > 1;
    let total_battles = battle_ids.len();

    let mut stored_battles = Vec::new();
    let mut overwrite_all_snapshots = false;
    let mut auto_continue = false;

    let mut live_prices: HashMap<(String, u8), u64> = HashMap::new();
    let mut recent_trades: HashMap<(String, u8), u64> = HashMap::new();

    for (idx, &bid) in battle_ids.iter().enumerate() {
        println!(
            "\n==================== [ {}/{} ] Battle {} ====================",
            idx + 1,
            total_battles,
            bid
        );
        let (mut stored, from_cache) = load_or_fetch_battle(client, bid, db, force_refresh).await?;
        let src_note = if from_cache {
            "本地库"
        } else {
            "官服最新"
        };
        println!(
            "  战斗加载完成 ({src_note}, 开始: {}, 击杀: {} 条)",
            short_time(&stored.summary.start_time),
            stored.events.len()
        );

        detect_alliance_from_battles(&[stored.clone()], db);

        let mut use_fresh_for_this = false;
        if update_prices && !stored.events.is_empty() {
            let should_update = if stored.price_snapshot.is_empty() {
                true
            } else {
                confirm_overwrite_snapshot(
                    bid,
                    stored.price_snapshot.len(),
                    stored.price_snapshot_at,
                    &mut overwrite_all_snapshots,
                )
            };

            if should_update {
                let (names_added, live_up, trade_up) = update_single_battle_prices(
                    client,
                    &mut stored,
                    db,
                    &mut live_prices,
                    &mut recent_trades,
                )
                .await?;
                use_fresh_for_this = true;
                println!(
                    "  快照已更新并保存 (中文名+{}, 买价+{}, 成交+{}, 快照共 {} 条)",
                    names_added,
                    live_up,
                    trade_up,
                    stored.price_snapshot.len()
                );
            } else {
                println!(
                    "  已保留本场原历史快照 ({} 条)，不覆盖。",
                    stored.price_snapshot.len()
                );
            }
        } else if stored.price_snapshot.is_empty() && !stored.events.is_empty() {
            // 首次分析但未传 -u：把当前本地库已知价格冻结成该场初始快照并存库
            let empty_live = HashMap::new();
            let empty_trade = HashMap::new();
            let snap = build_battle_snapshot(&stored.events, &empty_live, &empty_trade, db);
            if !snap.is_empty() {
                stored.price_snapshot = snap;
                stored.price_snapshot_at = Some(now_unix());
            }
            db.battles.insert(bid, stored.clone());
            save_db(db);
        } else if !from_cache {
            save_db(db);
        }

        // 多场模式下，每跑完一场立刻先打印本场独立战报，方便边看边等
        if is_multi {
            let single_snaps: Vec<Option<&HashMap<String, PriceRecord>>> =
                vec![Some(&stored.price_snapshot); stored.events.len()];
            let (empty_l, empty_t) = (HashMap::new(), HashMap::new());
            let (use_l, use_t) = if use_fresh_for_this {
                (&live_prices, &recent_trades)
            } else {
                (&empty_l, &empty_t)
            };
            render_battle_report(
                &format!("本场简报 Battle {bid}"),
                &[stored.clone()],
                &stored.events,
                &single_snaps,
                use_l,
                use_t,
                db,
                false, // 多场逐条打印时先不展开全员死亡列表，保持每场一屏看完
                show_our_details,
                false,
            );
        }

        stored_battles.push(stored);

        if is_multi && idx + 1 < total_battles && !auto_continue {
            print!(
                "\n── 第 {}/{} 场已存库。按 [Enter] 继续下一场 | a 自动跑完剩余 | q 提前停止看合并总账: ",
                idx + 1,
                total_battles
            );
            let _ = io::stdout().flush();
            let mut ans = String::new();
            io::stdin().read_line(&mut ans)?;
            let t = ans.trim();
            if t.eq_ignore_ascii_case("q") {
                break;
            } else if t.eq_ignore_ascii_case("a") {
                auto_continue = true;
            }
        }
    }

    if stored_battles.is_empty() {
        return Ok(());
    }

    let alliance_label_cfg = db
        .manual_alliance
        .as_deref()
        .or(db.my_alliance_name.as_deref());
    println!(
        "\n我方公会: {}   我方联盟: {}",
        DEFAULT_GUILD_NAME,
        alliance_label_cfg.unwrap_or("未检测到/无联盟 (可用 --set-alliance 设置)")
    );

    // 组装最终单场报告 / 多场合并总账（每条事件严格使用自己所属场次的独立价格快照！）
    let mut seen_event_ids = HashSet::new();
    let mut merged_events = Vec::new();
    let mut merged_event_snaps: Vec<Option<&HashMap<String, PriceRecord>>> = Vec::new();

    for sb in &stored_battles {
        for ev in &sb.events {
            if seen_event_ids.insert(ev.event_id) {
                merged_events.push(ev.clone());
                merged_event_snaps.push(Some(&sb.price_snapshot));
            }
        }
    }

    // 合并后按时间重新排序，保持事件与各自快照引用一一对应
    let mut paired: Vec<(KillEvent, Option<&HashMap<String, PriceRecord>>)> =
        merged_events.into_iter().zip(merged_event_snaps).collect();
    paired.sort_by(|a, b| a.0.time_stamp.cmp(&b.0.time_stamp));
    let (sorted_events, sorted_snaps): (Vec<_>, Vec<_>) = paired.into_iter().unzip();

    let final_heading = if stored_battles.len() == 1 {
        "一、本场战斗全部死亡名单".to_string()
    } else {
        format!(
            "\n==================== 多场合并总账（共 {} 场） ====================\n一、合并全部死亡汇总",
            stored_battles.len()
        )
    };

    let empty_live = HashMap::new();
    let empty_trade = HashMap::new();
    render_battle_report(
        &final_heading,
        &stored_battles,
        &sorted_events,
        &sorted_snaps,
        &empty_live,
        &empty_trade,
        db,
        !is_multi || show_all_details,
        show_our_details,
        show_all_details,
    );

    if !show_our_details && !show_all_details {
        println!("\n提示：默认隐藏逐件装备明细；加 -d 显示我方阵亡装备，加 -da 显示全员明细。");
    }
    println!(
        "说明：死者和 Event ID 可按 Ctrl+点击直达官网 | 每场价格已锁定为各自独立快照 | 本地库: {}",
        db_path().display()
    );

    Ok(())
}

pub fn show_stored_history_stats(db: &LocalDb) {
    if db.battles.is_empty() {
        println!("\n本地数据库尚未存档任何战斗，请先分析任意几场战斗。");
        return;
    }

    let empty_live = HashMap::new();
    let empty_trade = HashMap::new();
    let mut battles: Vec<&StoredBattle> = db.battles.values().collect();
    battles.sort_by(|a, b| b.summary.start_time.cmp(&a.summary.start_time));

    println!(
        "\n本地历史存档战斗汇总（共 {} 场，使用每场独立快照）\n",
        battles.len()
    );
    let mut b_table = new_clean_table();
    b_table.set_header(vec![
        hdr("Battle ID", false),
        hdr("开始时间", false),
        hdr("总击杀", true),
        hdr("我方阵亡", true),
        hdr("整场全套掉落", true),
        hdr("整场包内掉落", true),
        hdr("我方全套战损", true),
        hdr("物价快照", false),
    ]);

    let mut roster: HashMap<String, GuildMemberStats> = HashMap::new();
    let mut seen_events = HashSet::new();

    for sb in &battles {
        let mut battle_full = 0u64;
        let mut battle_missing = 0usize;
        let mut battle_approx = false;
        let mut battle_inv = 0u64;
        let mut battle_inv_missing = 0usize;
        let mut battle_inv_approx = false;
        let mut our_deaths = 0u32;
        let mut our_full = 0u64;
        let mut our_missing = 0usize;
        let mut our_approx = false;
        let mut attended = HashSet::new();

        if let Some(players) = &sb.summary.players {
            for bp in players.values() {
                if is_my_guild(bp.guild_id.as_deref(), bp.guild_name.as_deref()) {
                    attended.insert(bp.name.clone());
                    let entry = roster
                        .entry(bp.name.clone())
                        .or_insert_with(|| GuildMemberStats {
                            name: bp.name.clone(),
                            ..Default::default()
                        });
                    entry.kills += bp.kills.unwrap_or(0);
                }
            }
        }

        let snap = Some(&sb.price_snapshot);
        for ev in &sb.events {
            if !seen_events.insert(ev.event_id) {
                continue;
            }
            let eval = evaluate_equipment(
                ev.victim.equipment.as_ref(),
                &empty_live,
                &empty_trade,
                snap,
                db,
            );
            let inv_eval = evaluate_inventory(
                ev.victim.inventory.as_ref(),
                &empty_live,
                &empty_trade,
                snap,
                db,
            );
            battle_full += eval.full_cost;
            battle_missing += eval.total_missing;
            battle_approx |= eval.total_approx;
            battle_inv += inv_eval.inv_cost;
            battle_inv_missing += inv_eval.inv_missing;
            battle_inv_approx |= inv_eval.inv_approx;

            if is_my_guild(
                ev.killer.guild_id.as_deref(),
                ev.killer.guild_name.as_deref(),
            ) {
                attended.insert(ev.killer.name.clone());
            }
            if is_my_guild(
                ev.victim.guild_id.as_deref(),
                ev.victim.guild_name.as_deref(),
            ) {
                attended.insert(ev.victim.name.clone());
                our_deaths += 1;
                our_full += eval.full_cost;
                our_missing += eval.total_missing;
                our_approx |= eval.total_approx;

                let entry =
                    roster
                        .entry(ev.victim.name.clone())
                        .or_insert_with(|| GuildMemberStats {
                            name: ev.victim.name.clone(),
                            ..Default::default()
                        });
                entry.deaths += 1;
                entry.cross_total += eval.cross_cost;
                entry.full_total += eval.full_cost;
                entry.inv_total += inv_eval.inv_cost;
                entry.cross_missing += eval.cross_missing;
                entry.total_missing += eval.total_missing;
                entry.inv_missing += inv_eval.inv_missing;
                entry.cross_approx |= eval.cross_approx;
                entry.total_approx |= eval.total_approx;
                entry.inv_approx |= inv_eval.inv_approx;
            }
            if let Some(parts) = &ev.participants {
                for p in parts {
                    if is_my_guild(p.guild_id.as_deref(), p.guild_name.as_deref()) {
                        attended.insert(p.name.clone());
                        let entry =
                            roster
                                .entry(p.name.clone())
                                .or_insert_with(|| GuildMemberStats {
                                    name: p.name.clone(),
                                    ..Default::default()
                                });
                        if p.name != ev.killer.name {
                            entry.assists += 1;
                        }
                        entry.total_damage += p.damage_done.unwrap_or(0.0);
                        entry.total_healing += p.support_healing_done.unwrap_or(0.0);
                    }
                }
            }
            if let Some(gms) = &ev.group_members {
                for gm in gms {
                    if is_my_guild(gm.guild_id.as_deref(), gm.guild_name.as_deref()) {
                        attended.insert(gm.name.clone());
                        roster
                            .entry(gm.name.clone())
                            .or_insert_with(|| GuildMemberStats {
                                name: gm.name.clone(),
                                ..Default::default()
                            });
                    }
                }
            }
        }

        for pname in attended {
            if let Some(entry) = roster.get_mut(&pname) {
                entry.battles_attended += 1;
            }
        }

        let snap_cell = if !sb.price_snapshot.is_empty() {
            let t = sb
                .price_snapshot_at
                .map(short_unix_time)
                .unwrap_or_else(|| "已锁定".to_string());
            Cell::new(t).fg(Color::Green)
        } else {
            Cell::new("未锁定").fg(Color::Yellow)
        };

        b_table.add_row(vec![
            Cell::new(sb.summary.id.to_string()),
            Cell::new(short_time(&sb.summary.start_time)),
            cell_r(sb.events.len().to_string()),
            if our_deaths > 0 {
                cell_r(our_deaths.to_string()).fg(Color::Red)
            } else {
                cell_r("0")
            },
            format_cost_cell(battle_full, battle_missing, battle_approx),
            format_cost_cell(battle_inv, battle_inv_missing, battle_inv_approx),
            format_cost_cell(our_full, our_missing, our_approx),
            snap_cell,
        ]);
    }
    println!("{b_table}");

    let mut ranked: Vec<GuildMemberStats> = roster.into_values().collect();
    ranked.sort_by(|a, b| {
        b.battles_attended
            .cmp(&a.battles_attended)
            .then_with(|| b.deaths.cmp(&a.deaths))
            .then_with(|| b.full_total.cmp(&a.full_total))
            .then_with(|| b.total_healing.total_cmp(&a.total_healing))
    });

    println!(
        "\n公会「{}」历史累计出勤 & 战损总榜（共 {} 人）\n",
        DEFAULT_GUILD_NAME,
        ranked.len()
    );
    let mut r_table = new_clean_table();
    r_table.set_header(vec![
        hdr("排名", true),
        hdr("玩家", false),
        hdr("出勤", true),
        hdr("死亡", true),
        hdr("累计十字架", true),
        hdr("累计全套", true),
        hdr("累计包内", true),
        hdr("击杀", true),
        hdr("助攻", true),
        hdr("累计治疗", true),
        hdr("累计伤害", true),
    ]);
    for (idx, p) in ranked.iter().enumerate() {
        let heal = p.total_healing.round() as u64;
        r_table.add_row(vec![
            cell_r((idx + 1).to_string()),
            Cell::new(&p.name).add_attribute(Attribute::Bold),
            cell_r(p.battles_attended.to_string()),
            if p.deaths > 0 {
                cell_r(p.deaths.to_string()).fg(Color::Red)
            } else {
                cell_r("0")
            },
            format_cost_cell(p.cross_total, p.cross_missing, p.cross_approx),
            format_cost_cell(p.full_total, p.total_missing, p.total_approx),
            format_cost_cell(p.inv_total, p.inv_missing, p.inv_approx),
            cell_r(p.kills.to_string()),
            cell_r(p.assists.to_string()),
            if heal > 0 {
                cell_r(fmt_num(heal)).fg(Color::Green)
            } else {
                cell_r("0")
            },
            cell_r(fmt_num(p.total_damage.round() as u64)),
        ]);
    }
    println!("{r_table}");
}
