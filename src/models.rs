use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LocalDb {
    #[serde(default)]
    pub my_alliance_id: Option<String>,
    #[serde(default)]
    pub my_alliance_name: Option<String>,
    #[serde(default)]
    pub manual_alliance: Option<String>,
    #[serde(default)]
    pub ally_alliances: Vec<String>,
    #[serde(default)]
    pub ally_guilds: Vec<String>,
    #[serde(default)]
    pub item_names_zh: HashMap<String, String>,
    #[serde(default)]
    pub prices: HashMap<String, PriceRecord>,
    #[serde(default)]
    pub battles: HashMap<u64, StoredBattle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceRecord {
    pub price: u64,
    pub source: String,
    pub updated_at: u64,
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredBattle {
    pub summary: BattleSummary,
    pub events: Vec<KillEvent>,
    #[serde(default)]
    pub price_snapshot: HashMap<String, PriceRecord>,
    #[serde(default)]
    pub price_snapshot_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct BinItem {
    #[serde(rename = "UniqueName")]
    pub unique_name: Option<String>,
    #[serde(rename = "LocalizedNames")]
    pub localized_names: Option<HashMap<String, Option<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct GameInfoItemData {
    #[serde(alias = "localizedNames", alias = "LocalizedNames")]
    pub localized_names: Option<HashMap<String, Option<String>>>,
}

#[derive(Debug, Deserialize)]
pub struct GuildInfo {
    #[serde(default, alias = "AllianceId", alias = "allianceId")]
    pub alliance_id: Option<String>,
    #[serde(default, alias = "AllianceName", alias = "allianceName")]
    pub alliance_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BattleSummary {
    pub id: u64,
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "totalKills")]
    pub total_kills: u64,
    #[serde(default)]
    pub players: Option<HashMap<String, BattlePlayer>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BattlePlayer {
    pub name: String,
    #[serde(rename = "guildId")]
    pub guild_id: Option<String>,
    #[serde(rename = "guildName")]
    pub guild_name: Option<String>,
    #[serde(rename = "allianceId")]
    pub alliance_id: Option<String>,
    #[serde(rename = "allianceName")]
    pub alliance_name: Option<String>,
    pub kills: Option<u32>,
    pub deaths: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct KillEvent {
    pub event_id: u64,
    pub time_stamp: String,
    pub killer: EventPlayer,
    pub victim: EventPlayer,
    #[serde(default)]
    pub participants: Option<Vec<Participant>>,
    #[serde(default)]
    pub group_members: Option<Vec<EventPlayer>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct EventPlayer {
    pub id: Option<String>,
    pub name: String,
    pub guild_id: Option<String>,
    pub guild_name: Option<String>,
    pub alliance_id: Option<String>,
    pub alliance_name: Option<String>,
    pub average_item_power: Option<f64>,
    pub equipment: Option<Equipment>,
    pub inventory: Option<Vec<Option<EquipItem>>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Participant {
    pub id: Option<String>,
    pub name: String,
    pub guild_id: Option<String>,
    pub guild_name: Option<String>,
    pub alliance_id: Option<String>,
    pub alliance_name: Option<String>,
    pub average_item_power: Option<f64>,
    pub damage_done: Option<f64>,
    pub support_healing_done: Option<f64>,
    pub equipment: Option<Equipment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Equipment {
    pub main_hand: Option<EquipItem>,
    pub off_hand: Option<EquipItem>,
    pub head: Option<EquipItem>,
    pub armor: Option<EquipItem>,
    pub shoes: Option<EquipItem>,
    pub bag: Option<EquipItem>,
    pub cape: Option<EquipItem>,
    pub food: Option<EquipItem>,
    pub potion: Option<EquipItem>,
    pub mount: Option<EquipItem>,
}

impl Equipment {
    pub fn all_slots(&self) -> [(&'static str, bool, &Option<EquipItem>); 10] {
        [
            ("主手", true, &self.main_hand),
            ("副手", true, &self.off_hand),
            ("头盔", true, &self.head),
            ("胸甲", true, &self.armor),
            ("鞋子", true, &self.shoes),
            ("背包", false, &self.bag),
            ("披风", false, &self.cape),
            ("食物", false, &self.food),
            ("药水", false, &self.potion),
            ("坐骑", false, &self.mount),
        ]
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct EquipItem {
    #[serde(rename = "Type")]
    pub item_type: Option<String>,
    pub quality: Option<u8>,
    pub count: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct AodpPrice {
    pub item_id: String,
    pub quality: u8,
    pub sell_price_min: u64,
    #[serde(default)]
    pub sell_price_min_date: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AodpHistoryResp {
    pub item_id: String,
    pub quality: u8,
    #[serde(default)]
    pub data: Vec<AodpHistoryPoint>,
}

#[derive(Debug, Deserialize)]
pub struct AodpHistoryPoint {
    pub item_count: u64,
    pub avg_price: f64,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Ours,
    Ally,
    Hostile,
}

#[derive(Debug, Clone)]
pub struct GearCost {
    pub slot: &'static str,
    pub is_cross: bool,
    pub item_id: String,
    pub quality: u8,
    pub count: u32,
    pub subtotal: u64,
    pub source_tag: String,
    pub is_missing: bool,
    pub is_approx: bool,
    pub is_untradable: bool,
}

#[derive(Debug, Clone)]
pub struct GearEval {
    pub cross_cost: u64,
    pub full_cost: u64,
    pub cross_missing: usize,
    pub total_missing: usize,
    pub cross_approx: bool,
    pub total_approx: bool,
    pub gears: Vec<GearCost>,
}

#[derive(Debug, Clone)]
pub struct InvEval {
    pub inv_cost: u64,
    pub inv_missing: usize,
    pub inv_approx: bool,
    pub items: Vec<GearCost>,
}

#[derive(Debug, Clone)]
pub struct DeathRecord {
    pub event_id: u64,
    pub timestamp: String,
    pub killer_display: String,
    pub cross_cost: u64,
    pub full_cost: u64,
    pub inv_cost: u64,
    pub cross_missing: usize,
    pub total_missing: usize,
    pub inv_missing: usize,
    pub cross_approx: bool,
    pub total_approx: bool,
    pub inv_approx: bool,
    pub gears: Vec<GearCost>,
    pub inv_items: Vec<GearCost>,
}

#[derive(Debug)]
pub struct AllianceLossAgg {
    pub alliance_display: String,
    pub relation: Relation,
    pub guilds: HashSet<String>,
    pub participants: HashSet<String>,
    pub deaths: u32,
    pub cross_cost: u64,
    pub full_cost: u64,
    pub inv_cost: u64,
    pub cross_missing: usize,
    pub total_missing: usize,
    pub inv_missing: usize,
    pub cross_approx: bool,
    pub total_approx: bool,
    pub inv_approx: bool,
}

#[derive(Debug)]
pub struct GuildLossAgg {
    pub alliance_key: String,
    pub alliance_display: String,
    pub guild_name: String,
    pub relation: Relation,
    pub participants: HashSet<String>,
    pub deaths: u32,
    pub cross_cost: u64,
    pub full_cost: u64,
    pub inv_cost: u64,
    pub cross_missing: usize,
    pub total_missing: usize,
    pub inv_missing: usize,
    pub cross_approx: bool,
    pub total_approx: bool,
    pub inv_approx: bool,
}

#[derive(Debug, Default)]
pub struct GuildMemberStats {
    pub player_id: Option<String>,
    pub name: String,
    pub battles_attended: u32,
    pub deaths: u32,
    pub kills: u32,
    pub assists: u32,
    pub total_damage: f64,
    pub total_healing: f64,
    pub cross_total: u64,
    pub full_total: u64,
    pub inv_total: u64,
    pub cross_missing: usize,
    pub total_missing: usize,
    pub inv_missing: usize,
    pub cross_approx: bool,
    pub total_approx: bool,
    pub inv_approx: bool,
    pub main_hand: Option<String>,
    pub avg_ip: Option<f64>,
    pub death_records: Vec<DeathRecord>,
}

pub struct EvaluatedEvent {
    pub idx_original: usize,
    pub relation: Relation,
    pub alliance_key: String,
    pub alliance_display: String,
    pub guild_label: String,
    pub gear_eval: GearEval,
    pub inv_eval: InvEval,
}

#[derive(Debug, Clone)]
pub struct CombatantStats {
    pub player_id: Option<String>,
    pub name: String,
    pub guild_id: Option<String>,
    pub guild_name: Option<String>,
    pub alliance_id: Option<String>,
    pub alliance_name: Option<String>,
    pub relation: Relation,
    pub alliance_key: String,
    pub alliance_display: String,
    pub kills: u32,
    pub assists: u32,
    pub deaths: u32,
    pub heal_events: u32,
    pub total_damage: f64,
    pub total_healing: f64,
    pub max_single_healing: f64,
    pub main_hand: Option<String>,
    pub avg_ip: Option<f64>,
    pub sample_event_id: Option<u64>,
}

#[derive(Debug)]
pub struct AllianceHealAgg {
    pub alliance_display: String,
    pub relation: Relation,
    pub healer_count: u32,
    pub healer_deaths: u32,
    pub total_healing: f64,
    pub top_healer_name: String,
    pub top_healer_val: f64,
}
