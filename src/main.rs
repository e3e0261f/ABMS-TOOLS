mod api;
mod db;
mod filter;
mod models;
mod pricing;
mod render;
mod theme;

use crate::api::get_recent_guild_battles;
use crate::db::{db_path, load_db, save_db};
use crate::filter::DEFAULT_GUILD_ID;
use crate::render::{analyze_battles, print_battle_list, show_stored_history_stats};
use std::env;
use std::error::Error;
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = reqwest::Client::builder()
        .user_agent("ABMS-AlbionBattleTool/0.4")
        .build()?;

    let mut db = load_db();

    let mut arg_battle_ids: Vec<u64> = Vec::new();
    let mut show_our_details = false;
    let mut show_all_details = false;
    let mut force_refresh = false;
    let mut update_prices = false;
    let mut show_stats_only = false;
    let mut config_modified = false;
    let mut show_config = false;
    let mut list_limit = 10usize;

    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-d" | "--detail" => show_our_details = true,
            "-da" | "--detail-all" => {
                show_our_details = true;
                show_all_details = true;
            }
            "-r" | "--refresh" => force_refresh = true,
            "-u" | "--update" => update_prices = true,
            "-s" | "--stats" | "--history" => show_stats_only = true,
            "--show-config" => show_config = true,
            "--clear-ally" => {
                db.manual_alliance = None;
                db.ally_alliances.clear();
                db.ally_guilds.clear();
                config_modified = true;
                println!("已清空手动联盟与友军配置。");
            }
            "--set-alliance" => {
                if let Some(val) = args.get(i + 1) {
                    let trimmed = val.trim();
                    db.manual_alliance = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    };
                    config_modified = true;
                    println!("已设置我方联盟为: {:?}", db.manual_alliance);
                    i += 1;
                }
            }
            "--add-ally-alliance" => {
                if let Some(val) = args.get(i + 1) {
                    let v = val.trim().to_string();
                    if !v.is_empty() && !db.ally_alliances.contains(&v) {
                        db.ally_alliances.push(v.clone());
                        config_modified = true;
                        println!("已添加友军联盟: {v}");
                    }
                    i += 1;
                }
            }
            "--add-ally" | "--add-ally-guild" => {
                if let Some(val) = args.get(i + 1) {
                    let v = val.trim().to_string();
                    if !v.is_empty() && !db.ally_guilds.contains(&v) {
                        db.ally_guilds.push(v.clone());
                        config_modified = true;
                        println!("已添加友军公会: {v}");
                    }
                    i += 1;
                }
            }
            "-n" | "--limit" => {
                if let Some(next) = args.get(i + 1) {
                    if let Ok(n) = next.parse::<usize>() {
                        list_limit = n.clamp(1, 100);
                        i += 1;
                    }
                }
            }
            token => {
                if let Ok(id) = token.parse::<u64>() {
                    arg_battle_ids.push(id);
                }
            }
        }
        i += 1;
    }

    if config_modified {
        save_db(&db);
        if arg_battle_ids.is_empty() && !show_stats_only && !show_config {
            return Ok(());
        }
    }

    if show_config {
        println!(
            "本地库: {}\n自动联盟: {:?} ({:?})\n手动主联盟: {:?}\n友军联盟: {:?}\n友军公会: {:?}",
            db_path().display(),
            db.my_alliance_name,
            db.my_alliance_id,
            db.manual_alliance,
            db.ally_alliances,
            db.ally_guilds
        );
        return Ok(());
    }

    if show_stats_only {
        show_stored_history_stats(&db);
        return Ok(());
    }

    if !arg_battle_ids.is_empty() {
        analyze_battles(
            &client,
            &arg_battle_ids,
            &mut db,
            show_our_details,
            show_all_details,
            force_refresh,
            update_prices,
        )
        .await?;
        return Ok(());
    }

    let battles = get_recent_guild_battles(&client, DEFAULT_GUILD_ID, list_limit).await?;
    if battles.is_empty() {
        println!("未查询到最近战斗记录。");
        return Ok(());
    }

    print_battle_list(&battles, &db);

    print!(
        "\n请输入序号（如 1 或 1 2 合并，可附带 -u 更新物价 / -d 我方明细 / -da 全员明细）、s(历史总榜)、q(退出) [默认 1]: "
    );
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();

    if trimmed.eq_ignore_ascii_case("q") || trimmed.eq_ignore_ascii_case("quit") {
        return Ok(());
    }
    if trimmed.eq_ignore_ascii_case("s") || trimmed.eq_ignore_ascii_case("stats") {
        show_stored_history_stats(&db);
        return Ok(());
    }

    let mut selected_ids = Vec::new();
    if trimmed.is_empty() {
        selected_ids.push(battles[0].id);
    } else {
        for part in trimmed.split(|c: char| c.is_whitespace() || c == ',' || c == '，') {
            let p = part.trim();
            if p.is_empty() {
                continue;
            }
            match p {
                "-d" | "--detail" => show_our_details = true,
                "-da" | "--detail-all" => {
                    show_our_details = true;
                    show_all_details = true;
                }
                "-u" | "--update" => update_prices = true,
                "-r" | "--refresh" => force_refresh = true,
                _ => {
                    if let Ok(num) = p.parse::<u64>() {
                        if num >= 1 && (num as usize) <= battles.len() {
                            let bid = battles[(num as usize) - 1].id;
                            if !selected_ids.contains(&bid) {
                                selected_ids.push(bid);
                            }
                        } else if !selected_ids.contains(&num) {
                            selected_ids.push(num);
                        }
                    }
                }
            }
        }
    }

    if selected_ids.is_empty() {
        println!("未识别到有效的序号或 Battle ID。");
        return Ok(());
    }

    println!();
    analyze_battles(
        &client,
        &selected_ids,
        &mut db,
        show_our_details,
        show_all_details,
        force_refresh,
        update_prices,
    )
    .await?;
    Ok(())
}
