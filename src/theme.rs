use crate::filter::{base_item_id, parse_tier_and_enchant};
use crate::models::Relation;
use comfy_table::presets::NOTHING;
use comfy_table::{Attribute, Cell, CellAlignment, Color, Table};
use std::collections::HashMap;

pub fn tier_rgb(tier: u8) -> (u8, u8, u8) {
    match tier {
        1 => (176, 176, 176),
        2 => (166, 114, 72),
        3 => (88, 185, 71),
        4 => (74, 159, 232),
        5 => (218, 75, 67),
        6 => (242, 145, 38),
        7 => (245, 214, 61),
        8 => (255, 255, 255),
        _ => (128, 128, 128),
    }
}

pub fn format_item_display(item_id: &str, zh_names: &HashMap<String, String>) -> (u8, String) {
    let (tier, enchant) = parse_tier_and_enchant(item_id);
    let base = base_item_id(item_id);
    let zh = zh_names
        .get(item_id)
        .or_else(|| zh_names.get(base))
        .map(String::as_str)
        .unwrap_or(item_id);

    let label = if tier > 0 {
        format!("{tier}.{enchant} {zh}")
    } else {
        zh.to_string()
    };
    (tier, label)
}

pub fn item_cell(item_id: &str, zh_names: &HashMap<String, String>) -> Cell {
    let (tier, text) = format_item_display(item_id, zh_names);
    let (r, g, b) = tier_rgb(tier);
    let cell = Cell::new(text).fg(Color::Rgb { r, g, b });
    if tier == 8 {
        cell.add_attribute(Attribute::Bold)
    } else {
        cell
    }
}

pub fn killboard_url(event_id: u64) -> String {
    format!("https://albiononline.com/killboard/kill/{event_id}?server=live_sgp")
}

pub fn osc8_link(url: &str, label: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")
}

pub fn relation_cell(rel: Relation) -> Cell {
    match rel {
        Relation::Ours => Cell::new("我方")
            .fg(Color::Green)
            .add_attribute(Attribute::Bold),
        Relation::Ally => Cell::new("友盟")
            .fg(Color::Blue)
            .add_attribute(Attribute::Bold),
        Relation::Hostile => Cell::new("敌对")
            .fg(Color::Red)
            .add_attribute(Attribute::Bold),
    }
}

pub fn apply_relation_color(cell: Cell, rel: Relation) -> Cell {
    match rel {
        Relation::Ours => cell.fg(Color::Green).add_attribute(Attribute::Bold),
        Relation::Ally => cell.fg(Color::Blue),
        Relation::Hostile => cell.fg(Color::Red),
    }
}

pub fn new_clean_table() -> Table {
    let mut table = Table::new();
    table.load_style(NOTHING);
    table
}

pub fn hdr(text: &str, right_align: bool) -> Cell {
    let cell = Cell::new(text)
        .add_attribute(Attribute::Bold)
        .fg(Color::Cyan);
    if right_align {
        cell.set_alignment(CellAlignment::Right)
    } else {
        cell
    }
}

pub fn cell_r(text: impl Into<String>) -> Cell {
    Cell::new(text.into()).set_alignment(CellAlignment::Right)
}

pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (idx, ch) in s.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

pub fn format_cost_text(cost: u64, missing: usize, approx: bool) -> String {
    if missing > 0 {
        format!("{} (缺{missing})", fmt_num(cost))
    } else if approx && cost > 0 {
        format!("{} ~", fmt_num(cost))
    } else {
        fmt_num(cost)
    }
}

pub fn format_cost_cell(cost: u64, missing: usize, approx: bool) -> Cell {
    let text = format_cost_text(cost, missing, approx);
    let cell = cell_r(text);
    if missing > 0 {
        cell.fg(Color::Yellow).add_attribute(Attribute::Bold)
    } else if approx && cost > 0 {
        cell.fg(Color::Rgb {
            r: 215,
            g: 190,
            b: 110,
        })
    } else {
        cell
    }
}

pub fn short_time(ts: &str) -> String {
    if ts.len() >= 19 {
        format!("{} {}", &ts[5..10], &ts[11..19])
    } else {
        ts.to_string()
    }
}

pub fn short_hms(ts: &str) -> &str {
    if ts.len() >= 19 {
        &ts[11..19]
    } else {
        ts
    }
}

pub fn format_player_tag(name: &str, guild_name: Option<&str>) -> String {
    match guild_name.filter(|g| !g.is_empty()) {
        Some(g) => format!("{name} [{g}]"),
        None => name.to_string(),
    }
}
