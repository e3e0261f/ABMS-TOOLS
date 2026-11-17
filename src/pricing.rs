use crate::db::{now_unix, price_key};
use crate::filter::{equivalent_tier_siblings, is_untradable_item};
use crate::models::{
    EquipItem, Equipment, GearCost, GearEval, InvEval, KillEvent, LocalDb, PriceRecord,
};
use std::collections::HashMap;

pub fn lookup_exact_or_lower_quality(
    live_prices: &HashMap<(String, u8), u64>,
    recent_trades: &HashMap<(String, u8), u64>,
    battle_snapshot: Option<&HashMap<String, PriceRecord>>,
    db: &LocalDb,
    item_id: &str,
    q: u8,
    allow_higher_if_worn: bool,
) -> Option<(u64, String, bool)> {
    // 0. 手动锁定价优先
    if let Some(rec) = db.prices.get(&price_key(item_id, q)) {
        if rec.source == "manual" && rec.price > 0 {
            return Some((rec.price, "[手]".to_string(), false));
        }
    }
    // 1. 若本次主动更新了实时/近期价，优先使用新鲜价
    if let Some(&p) = live_prices.get(&(item_id.to_string(), q)) {
        return Some((p, String::new(), false));
    }
    if let Some(&p) = recent_trades.get(&(item_id.to_string(), q)) {
        return Some((p, "[近]".to_string(), true));
    }
    // 2. 使用该场战斗自己冻结的价格快照（防止未来物价波动改写历史）
    if let Some(snap) = battle_snapshot {
        if let Some(rec) = snap.get(&price_key(item_id, q)) {
            if rec.price > 0 {
                let tag = rec.tag.clone().unwrap_or_else(|| "[快照]".to_string());
                let is_approx = rec.source != "live" && rec.source != "manual";
                return Some((rec.price, tag, is_approx));
            }
        }
    }
    // 3. 回退全局本地库
    if let Some(rec) = db.prices.get(&price_key(item_id, q)) {
        if rec.price > 0 {
            let tag = if rec.source == "recent_trade" {
                "[近库]"
            } else {
                "[库]"
            };
            return Some((rec.price, tag.to_string(), true));
        }
    }

    // 4. 同装备向下查普通品质
    let max_fallback_q = if allow_higher_if_worn { q } else { q.min(3) };
    let mut lower_cands = Vec::new();
    for cand in [q.saturating_sub(1), 3, 2, 1] {
        if cand >= 1 && cand != q && cand <= max_fallback_q && !lower_cands.contains(&cand) {
            lower_cands.push(cand);
        }
    }
    for fq in lower_cands {
        if let Some(&p) = live_prices.get(&(item_id.to_string(), fq)) {
            return Some((p, format!("≈Q{fq}"), true));
        }
        if let Some(&p) = recent_trades.get(&(item_id.to_string(), fq)) {
            return Some((p, format!("≈Q{fq}"), true));
        }
        if let Some(snap) = battle_snapshot {
            if let Some(rec) = snap.get(&price_key(item_id, fq)) {
                if rec.price > 0 {
                    return Some((rec.price, format!("≈Q{fq}"), true));
                }
            }
        }
        if let Some(rec) = db.prices.get(&price_key(item_id, fq)) {
            if rec.price > 0 {
                return Some((rec.price, format!("≈Q{fq}"), true));
            }
        }
    }

    None
}

fn lookup_sibling_price(
    live_prices: &HashMap<(String, u8), u64>,
    recent_trades: &HashMap<(String, u8), u64>,
    db: &LocalDb,
    sib_id: &str,
    target_q: u8,
) -> Option<u64> {
    let mut q_order = Vec::new();
    if (1..=3).contains(&target_q) {
        q_order.push(target_q);
    }
    for q in [3u8, 2, 1] {
        if !q_order.contains(&q) {
            q_order.push(q);
        }
    }
    if target_q >= 4 {
        q_order.insert(0, target_q);
    }

    for q in q_order {
        if let Some(&p) = live_prices.get(&(sib_id.to_string(), q)) {
            return Some(p);
        }
        if let Some(&p) = recent_trades.get(&(sib_id.to_string(), q)) {
            return Some(p);
        }
        if let Some(rec) = db.prices.get(&price_key(sib_id, q)) {
            if rec.price > 0 {
                return Some(rec.price);
            }
        }
    }
    None
}

/// 返回: (单价, 来源标签, 是否无报价, 是否估算, 是否不可交易)
pub fn lookup_price(
    live_prices: &HashMap<(String, u8), u64>,
    recent_trades: &HashMap<(String, u8), u64>,
    battle_snapshot: Option<&HashMap<String, PriceRecord>>,
    db: &LocalDb,
    item_id: &str,
    quality: Option<u8>,
) -> (u64, String, bool, bool, bool) {
    if is_untradable_item(item_id) {
        return (0, "[非卖]".to_string(), false, false, true);
    }

    let q = quality.unwrap_or(1).max(1);

    if let Some((p, tag, approx)) = lookup_exact_or_lower_quality(
        live_prices,
        recent_trades,
        battle_snapshot,
        db,
        item_id,
        q,
        true,
    ) {
        return (p, tag, false, approx, false);
    }

    let (equiv, siblings) = equivalent_tier_siblings(item_id);
    if !siblings.is_empty() {
        let sib_prices: Vec<u64> = siblings
            .iter()
            .filter_map(|sib| lookup_sibling_price(live_prices, recent_trades, db, sib, q))
            .collect();
        if !sib_prices.is_empty() {
            let sum: u64 = sib_prices.iter().sum();
            let avg = sum / (sib_prices.len() as u64);
            return (
                avg,
                format!("≈T{equiv}({})", sib_prices.len()),
                false,
                true,
                false,
            );
        }
    }

    for fq in [4u8, 5] {
        if fq == q {
            continue;
        }
        if let Some(&p) = live_prices.get(&(item_id.to_string(), fq)) {
            return (p, format!("≈Q{fq}"), false, true, false);
        }
        if let Some(&p) = recent_trades.get(&(item_id.to_string(), fq)) {
            return (p, format!("≈Q{fq}"), false, true, false);
        }
        if let Some(rec) = db.prices.get(&price_key(item_id, fq)) {
            if rec.price > 0 {
                return (rec.price, format!("≈Q{fq}"), false, true, false);
            }
        }
    }

    (0, String::new(), true, false, false)
}

pub fn evaluate_equipment(
    eq: Option<&Equipment>,
    live_prices: &HashMap<(String, u8), u64>,
    recent_trades: &HashMap<(String, u8), u64>,
    battle_snapshot: Option<&HashMap<String, PriceRecord>>,
    db: &LocalDb,
) -> GearEval {
    let Some(eq) = eq else {
        return GearEval {
            cross_cost: 0,
            full_cost: 0,
            cross_missing: 0,
            total_missing: 0,
            cross_approx: false,
            total_approx: false,
            gears: Vec::new(),
        };
    };

    let mut cross_cost = 0u64;
    let mut extra_cost = 0u64;
    let mut cross_missing = 0usize;
    let mut extra_missing = 0usize;
    let mut cross_approx = false;
    let mut extra_approx = false;
    let mut gears = Vec::new();

    for (slot_name, is_cross, maybe_item) in eq.all_slots() {
        let Some(item) = maybe_item else { continue };
        let Some(item_id) = &item.item_type else {
            continue;
        };

        let count = item.count.unwrap_or(1).max(1);
        let quality = item.quality.unwrap_or(1);
        let (unit_price, source_tag, is_missing, is_approx, is_untradable) = lookup_price(
            live_prices,
            recent_trades,
            battle_snapshot,
            db,
            item_id,
            item.quality,
        );
        let subtotal = unit_price * count as u64;

        if is_cross {
            cross_cost += subtotal;
            if is_missing {
                cross_missing += 1;
            }
            if is_approx {
                cross_approx = true;
            }
        } else {
            extra_cost += subtotal;
            if is_missing {
                extra_missing += 1;
            }
            if is_approx {
                extra_approx = true;
            }
        }

        gears.push(GearCost {
            slot: slot_name,
            is_cross,
            item_id: item_id.clone(),
            quality,
            count,
            subtotal,
            source_tag,
            is_missing,
            is_approx,
            is_untradable,
        });
    }

    GearEval {
        cross_cost,
        full_cost: cross_cost + extra_cost,
        cross_missing,
        total_missing: cross_missing + extra_missing,
        cross_approx,
        total_approx: cross_approx || extra_approx,
        gears,
    }
}

pub fn evaluate_inventory(
    inv: Option<&Vec<Option<EquipItem>>>,
    live_prices: &HashMap<(String, u8), u64>,
    recent_trades: &HashMap<(String, u8), u64>,
    battle_snapshot: Option<&HashMap<String, PriceRecord>>,
    db: &LocalDb,
) -> InvEval {
    let Some(raw_items) = inv else {
        return InvEval {
            inv_cost: 0,
            inv_missing: 0,
            inv_approx: false,
            items: Vec::new(),
        };
    };

    let mut merged: HashMap<(String, u8), u32> = HashMap::new();
    for entry in raw_items.iter().flatten() {
        let Some(id) = &entry.item_type else { continue };
        let q = entry.quality.unwrap_or(1);
        let cnt = entry.count.unwrap_or(1).max(1);
        *merged.entry((id.clone(), q)).or_default() += cnt;
    }

    let mut inv_cost = 0u64;
    let mut inv_missing = 0usize;
    let mut inv_approx = false;
    let mut items = Vec::with_capacity(merged.len());

    for ((item_id, quality), count) in merged {
        let (unit_price, source_tag, is_missing, is_approx, is_untradable) = lookup_price(
            live_prices,
            recent_trades,
            battle_snapshot,
            db,
            &item_id,
            Some(quality),
        );
        let subtotal = unit_price * count as u64;
        inv_cost += subtotal;
        if is_missing {
            inv_missing += 1;
        }
        if is_approx {
            inv_approx = true;
        }
        items.push(GearCost {
            slot: "背包内",
            is_cross: false,
            item_id,
            quality,
            count,
            subtotal,
            source_tag,
            is_missing,
            is_approx,
            is_untradable,
        });
    }

    items.sort_by(|a, b| {
        b.subtotal
            .cmp(&a.subtotal)
            .then_with(|| a.item_id.cmp(&b.item_id))
    });

    InvEval {
        inv_cost,
        inv_missing,
        inv_approx,
        items,
    }
}

/// 为单场战斗冻结一套完整价格快照（记录当时算出的每个物品单价及标签）
pub fn build_battle_snapshot(
    events: &[KillEvent],
    live_prices: &HashMap<(String, u8), u64>,
    recent_trades: &HashMap<(String, u8), u64>,
    db: &LocalDb,
) -> HashMap<String, PriceRecord> {
    let now = now_unix();
    let mut snap = HashMap::new();

    for ev in events {
        if let Some(eq) = &ev.victim.equipment {
            for (_, _, maybe_item) in eq.all_slots() {
                if let Some(item) = maybe_item {
                    if let Some(id) = &item.item_type {
                        let q = item.quality.unwrap_or(1).max(1);
                        let key = price_key(id, q);
                        if !snap.contains_key(&key) {
                            let (p, tag, missing, approx, untradable) =
                                lookup_price(live_prices, recent_trades, None, db, id, Some(q));
                            if !missing && !untradable && p > 0 {
                                snap.insert(
                                    key,
                                    PriceRecord {
                                        price: p,
                                        source: if approx {
                                            "approx".to_string()
                                        } else {
                                            "live".to_string()
                                        },
                                        updated_at: now,
                                        tag: if tag.is_empty() {
                                            Some("[快照]".to_string())
                                        } else {
                                            Some(tag)
                                        },
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        if let Some(inv) = &ev.victim.inventory {
            for item in inv.iter().flatten() {
                if let Some(id) = &item.item_type {
                    let q = item.quality.unwrap_or(1).max(1);
                    let key = price_key(id, q);
                    if !snap.contains_key(&key) {
                        let (p, tag, missing, approx, untradable) =
                            lookup_price(live_prices, recent_trades, None, db, id, Some(q));
                        if !missing && !untradable && p > 0 {
                            snap.insert(
                                key,
                                PriceRecord {
                                    price: p,
                                    source: if approx {
                                        "approx".to_string()
                                    } else {
                                        "live".to_string()
                                    },
                                    updated_at: now,
                                    tag: if tag.is_empty() {
                                        Some("[快照]".to_string())
                                    } else {
                                        Some(tag)
                                    },
                                },
                            );
                        }
                    }
                }
            }
        }
    }

    snap
}
