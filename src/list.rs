use std::path::Path;

use anyhow::Result;

/// `tape list`：列出数据目录下缓存的站点，以及每个站点的接口快照数、资源文件数。
pub fn run(dir: &Path) -> Result<()> {
    let snap_dir = dir.join("snapshots");
    if !snap_dir.is_dir() {
        println!(
            "数据目录 {} 下没有 snapshots/，请先运行 tape record",
            dir.display()
        );
        return Ok(());
    }
    let res_dir = dir.join("resources");

    let mut rows: Vec<(String, usize, usize)> = Vec::new();
    for entry in std::fs::read_dir(&snap_dir)? {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let site = entry.file_name().to_string_lossy().into_owned();
        let api_count = count_json_files(&entry.path());
        let asset_count = site_asset_count(&res_dir, &site);
        rows.push((site, api_count, asset_count));
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    println!("{:<26} {:>7} {:>8}", "站点", "接口", "资源");
    let mut total_api = 0usize;
    let mut total_asset = 0usize;
    for (site, api, asset) in &rows {
        println!("{:<26} {:>7} {:>8}", site, api, asset);
        total_api += api;
        total_asset += asset;
    }
    println!("{:<26} {:>7} {:>8}", "合计", total_api, total_asset);
    Ok(())
}

/// 统计目录下的快照 JSON 文件数。
fn count_json_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(Result::ok)
                .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
        .unwrap_or(0)
}

/// 统计站点在 resources/ 下的资源文件数（硬链接副本，递归计数；blobs/ 去重库不计入）。
fn site_asset_count(res_dir: &Path, site: &str) -> usize {
    let site_dir = res_dir.join(site);
    if !site_dir.is_dir() {
        return 0;
    }
    let mut total = 0usize;
    let mut stack = vec![site_dir];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                total += 1;
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_json_snapshot_files() {
        let root = std::env::temp_dir().join(format!("tape-list-test-{}", std::process::id()));
        let site = root.join("snapshots").join("a.com");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("001-GET-x.json"), "{}").unwrap();
        std::fs::write(site.join("002-POST-y.json"), "{}").unwrap();
        std::fs::write(site.join("readme.txt"), "x").unwrap();
        assert_eq!(count_json_files(&site), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn counts_assets_recursively_and_ignores_missing_site() {
        let root = std::env::temp_dir().join(format!("tape-list-res-{}", std::process::id()));
        let site = root.join("resources").join("b.com");
        std::fs::create_dir_all(site.join("img")).unwrap();
        std::fs::write(site.join("img").join("a.png"), "x").unwrap();
        std::fs::write(site.join("index.html"), "x").unwrap();
        assert_eq!(site_asset_count(&root.join("resources"), "b.com"), 2);
        // 未落盘资源的站点返回 0；blobs/ 去重库不计入任何站点
        assert_eq!(site_asset_count(&root.join("resources"), "c.com"), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
