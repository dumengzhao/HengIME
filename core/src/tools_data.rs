//! 工具面板数据源（表情 / 符号）。
//!
//! **静态直读**：只读 `<shared_data_dir>/opencc/emoji.txt` 与
//! `symbols_v.yaml`，不碰 librime 会话——工具窗口在用户打字过程中常驻，
//! 若去查 librime 会污染当前输入状态。数据来自上游雾凇拼音配置。
//!
//! 结构统一为 `ToolTab { groups }`，Slint 侧按 tab 懒加载、分页渲染；
//! 后续加「常用语 / 颜文字」只是新增一个 `ToolTab` + provider。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolGroup {
    pub label: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ToolTab {
    pub name: String,
    pub groups: Vec<ToolGroup>,
}

impl ToolTab {
    /// 展平成一条连续列表（按组顺序），供分页切片。
    pub fn flat_items(&self) -> Vec<String> {
        self.groups.iter().flat_map(|g| g.items.iter().cloned()).collect()
    }
}

/// 载入全部 tab。文件缺失时返回空组（不 panic、不阻塞 UI 线程）。
pub fn load_tools(shared_dir: &Path) -> Vec<ToolTab> {
    vec![
        load_emoji(shared_dir),
        load_symbols(&shared_dir.join("symbols_v.yaml")),
    ]
}

/// 彩色 emoji 雪碧图（运行时解码一次，缓存于 UI 线程）。
/// 像素为 RGBA8 直乘（非预乘）；贴帧时再做 source-over 混合。
pub struct EmojiSheet {
    pub pixels: Vec<u8>,
    pub w: usize,
    pub h: usize,
    pub cell: usize,
    pub cols: usize,
    pub count: usize,
    /// 字符 → 雪碧图格子下标。最近使用 tab 混排表情与符号，必须按字符查，
    /// 不能沿用「页下标」（那只对顺序固定的表情 tab 成立）。
    index_by_char: HashMap<String, usize>,
}

impl EmojiSheet {
    /// 取第 idx 个 emoji 的左上角坐标（越界返回 None）。
    pub fn origin(&self, idx: usize) -> Option<(usize, usize)> {
        if idx >= self.count {
            return None;
        }
        Some(((idx % self.cols) * self.cell, (idx / self.cols) * self.cell))
    }

    /// 按字符查位图下标（符号 tab 的普通文字字符返回 None）。
    pub fn lookup(&self, s: &str) -> Option<usize> {
        self.index_by_char.get(s).copied()
    }

    /// 表情 tab 用：第 idx 项直接就是雪碧图第 idx 格（顺序由 JSON 保证）。
    pub fn index_of_flat(&self, idx: usize) -> Option<usize> {
        (idx < self.count).then_some(idx)
    }
}

#[derive(serde::Deserialize)]
struct SheetMeta {
    cell: usize,
    cols: usize,
    count: usize,
    groups: Vec<ToolGroup>,
}

/// 读雪碧图元数据（分组顺序即 flat_items 顺序，保证下标一一对应）。
fn sheet_meta(shared_dir: &Path) -> Option<SheetMeta> {
    let text = std::fs::read_to_string(shared_dir.join("others").join("emoji-sheet.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 解码 PNG 雪碧图。文件缺失/损坏返回 None（工具窗退化到文字渲染，不崩）。
pub fn load_emoji_sheet(shared_dir: &Path) -> Option<EmojiSheet> {
    let meta = sheet_meta(shared_dir)?;
    let mut index_by_char = HashMap::with_capacity(meta.count);
    let mut idx = 0usize;
    for g in &meta.groups {
        for t in &g.items {
            index_by_char.entry(t.clone()).or_insert(idx);
            idx += 1;
        }
    }
    let file = std::fs::File::open(shared_dir.join("others").join("emoji-sheet.png")).ok()?;
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut pixels).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    pixels.truncate(info.buffer_size());
    Some(EmojiSheet {
        pixels,
        w: info.width as usize,
        h: info.height as usize,
        cell: meta.cell,
        cols: meta.cols,
        count: meta.count,
        index_by_char,
    })
}

/// 最近使用：表情窗与符号窗各存一份（互不污染），最新在前、去重、上限一页。
/// 落 `<user_data_dir>/.heng/recent-{emoji,symbols}.json`（原子写）。
pub const RECENT_MAX: usize = VIEW_CELLS;

/// 常用列表归属哪个窗口
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RecentKind {
    Emoji,
    Symbols,
}

impl RecentKind {
    fn file(self) -> &'static str {
        match self {
            RecentKind::Emoji => "recent-emoji.json",
            RecentKind::Symbols => "recent-symbols.json",
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct RecentFile {
    items: Vec<String>,
}

pub struct RecentStore {
    path: PathBuf,
    items: Vec<String>,
}

impl RecentStore {
    pub fn new(user_dir: &Path, kind: RecentKind) -> Self {
        let path = user_dir.join(".heng").join(kind.file());
        let items = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<RecentFile>(&s).ok())
            .map(|f| {
                let mut seen = HashSet::new();
                f.items
                    .into_iter()
                    .filter(|s| !s.is_empty() && seen.insert(s.clone()))
                    .take(RECENT_MAX)
                    .collect()
            })
            .unwrap_or_default();
        Self { path, items }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// 作为 tab 0 的数据源；空列表时仍返回空 tab（UI 侧会回退到表情 tab）。
    pub fn tab(&self) -> ToolTab {
        ToolTab {
            name: "最近".into(),
            groups: vec![ToolGroup {
                label: "最近使用".into(),
                items: self.items.clone(),
            }],
        }
    }

    /// 记录一次使用：去重、置顶、截断、落盘。
    pub fn push(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.items.retain(|x| x != s);
        self.items.insert(0, s.to_string());
        self.items.truncate(RECENT_MAX);
        self.save();
    }

    fn save(&self) {
        let Some(dir) = self.path.parent() else { return };
        let _ = std::fs::create_dir_all(dir);
        let Ok(json) = serde_json::to_string(&RecentFile { items: self.items.clone() }) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

/// 表情含义表。
///
/// 两级来源：
/// 1. others/emoji-names-zh.json —— CLDR 官方中文短名（tools/gen-emoji-names.py 生成），
///    1975 条，覆盖绝大多数表情；
/// 2. opencc/emoji.txt —— 给 CLDR 缺失的补（ZWJ 组合、旗帜等）。注意它的左列是
///    「输入触发词」不是名字（❣️ 叫「心叹号」），所以只用来兜底。
pub fn load_emoji_meanings(shared_dir: &Path) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    // 1. CLDR 中文名
    if let Ok(text) = std::fs::read_to_string(
        shared_dir.join("others").join("emoji-names-zh.json"),
    ) {
        if let Ok(m) = serde_json::from_str::<HashMap<String, String>>(&text) {
            map = m;
        }
    }
    // 2. 兜底：emoji.txt 的触发词（只补没有的）
    if let Ok(text) = std::fs::read_to_string(shared_dir.join("opencc").join("emoji.txt")) {
        for line in text.lines() {
            let Some((word, rhs)) = line.split_once('\t') else { continue };
            let word = word.trim();
            if word.is_empty() {
                continue;
            }
            for (i, token) in rhs.split_whitespace().enumerate() {
                if i == 0 || token.is_empty() {
                    continue;
                }
                if token
                    .chars()
                    .all(|c| c.is_ascii() || ('\u{4e00}'..='\u{9fff}').contains(&c))
                {
                    continue;
                }
                let plain: String = token.chars().filter(|c| *c != '\u{fe0f}').collect();
                map.entry(token.to_string())
                    .or_insert_with(|| word.to_string());
                if plain != token {
                    map.entry(plain).or_insert_with(|| word.to_string());
                }
            }
        }
    }
    map
}

/// 查含义：先按原字符，再去掉变体选择符（U+FE0F）重试 —— 词表与 CLDR 的
/// 「⚠️ / ⚠」写法经常不一致。
pub fn meaning_of<'a>(
    map: &'a HashMap<String, String>,
    ch: &str,
) -> Option<&'a String> {
    if let Some(v) = map.get(ch) {
        return Some(v);
    }
    let plain: String = ch.chars().filter(|c| *c != '\u{fe0f}').collect();
    map.get(&plain)
}

/// 一次性迁移：旧版共用 recent-tools.json 按「能否在雪碧图里查到」拆分。
/// 已经有新文件时不动；迁移完把旧文件改名 .bak 保留。
pub fn migrate_recent(user_dir: &Path, sheet: Option<&EmojiSheet>) {
    let dir = user_dir.join(".heng");
    let old = dir.join("recent-tools.json");
    if !old.exists() {
        return;
    }
    let ne = dir.join(RecentKind::Emoji.file());
    let ns = dir.join(RecentKind::Symbols.file());
    if ne.exists() || ns.exists() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&old) else { return };
    let Ok(f) = serde_json::from_str::<RecentFile>(&text) else { return };
    let (mut e, mut s): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for it in f.items {
        if it.is_empty() {
            continue;
        }
        if sheet.map_or(false, |sh| sh.lookup(&it).is_some()) {
            e.push(it);
        } else {
            s.push(it);
        }
    }
    e.truncate(RECENT_MAX);
    s.truncate(RECENT_MAX);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(j) = serde_json::to_string(&RecentFile { items: e }) {
        let _ = std::fs::write(&ne, j);
    }
    if let Ok(j) = serde_json::to_string(&RecentFile { items: s }) {
        let _ = std::fs::write(&ns, j);
    }
    let _ = std::fs::rename(&old, dir.join("recent-tools.json.bak"));
}

/// 一屏可见的列数 × 行数。滚动不是分页：数据全量在内存里，
/// 每次只把「当前可见的 5 行」写进 Slint 模型（软件渲染器不做虚拟化，
/// 全量 6000+ 格会逐帧布局重绘，扛不住）。
/// 改动需同步 ui.rs 的 slint! 布局常量与位图步长。
pub const VIEW_COLS: usize = 10;
pub const VIEW_ROWS: usize = 6;
pub const VIEW_CELLS: usize = VIEW_COLS * VIEW_ROWS;

fn emoji_group(cp: u32) -> &'static str {
    match cp {
        0x1F600..=0x1F64F => "笑脸与情感",
        0x1F300..=0x1F3FF => "活动与物品",
        0x1F400..=0x1F4FF => "动物与自然",
        0x1F900..=0x1F9FF => "人物与身体",
        0x1F680..=0x1F6FF => "交通与地点",
        0x1F1E6..=0x1F1FF => "旗帜",
        0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x1F000..=0x1F2FF => "符号与旗帜",
        _ => "其他",
    }
}

fn load_emoji(shared_dir: &Path) -> ToolTab {
    // 优先用雪碧图元数据：分组/顺序与位图下标严格一致，避免两处解析漂移。
    // 未生成资源时回退解析 emoji.txt（单色文字渲染，功能仍在）。
    if let Some(meta) = sheet_meta(shared_dir) {
        if !meta.groups.is_empty() {
            return ToolTab {
                name: "表情".into(),
                groups: meta.groups,
            };
        }
    }
    let mut groups: Vec<ToolGroup> = Vec::new();
    let mut index: std::collections::HashMap<&'static str, usize> =
        std::collections::HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    if let Ok(text) = std::fs::read_to_string(shared_dir.join("opencc").join("emoji.txt")) {
        for line in text.lines() {
            // OpenCC 文本词典：`中文词\t中文词 emoji1 emoji2 ...`
            // 右侧第一项是原词，其后是候选 emoji（可能带 ZWJ 序列）。
            let Some((_, rhs)) = line.split_once('\t') else {
                continue;
            };
            for (i, token) in rhs.split_whitespace().enumerate() {
                if i == 0 || token.is_empty() {
                    continue; // 跳过原词
                }
                // 纯中文/ASCII 的说明文字（词表里偶有）不当作 emoji
                if token
                    .chars()
                    .all(|c| c.is_ascii() || ('\u{4e00}'..='\u{9fff}').contains(&c))
                {
                    continue;
                }
                if !seen.insert(token.to_string()) {
                    continue;
                }
                let cp = token.chars().next().map(|c| c as u32).unwrap_or(0);
                let label = emoji_group(cp);
                let gi = *index.entry(label).or_insert_with(|| {
                    groups.push(ToolGroup {
                        label: label.to_string(),
                        items: Vec::new(),
                    });
                    groups.len() - 1
                });
                groups[gi].items.push(token.to_string());
            }
        }
    }
    // 固定组顺序（HashMap 顺序不稳定，避免每次启动表情顺序漂移）
    const ORDER: [&str; 8] = [
        "笑脸与情感", "人物与身体", "动物与自然", "活动与物品",
        "交通与地点", "符号与旗帜", "旗帜", "其他",
    ];
    let mut ordered: Vec<ToolGroup> = Vec::new();
    for name in ORDER {
        if let Some(g) = groups.iter().find(|g| g.label == name && !g.items.is_empty()) {
            ordered.push(g.clone());
        }
    }
    ToolTab {
        name: "表情".into(),
        groups: ordered,
    }
}

/// 解析 symbols_v.yaml：`symbols:` 下每个键是一组；`vhelp`/`vlist` 提供
/// 键 → 中文名的映射（形如 `"vfh(符号)"`）。值可能是嵌套数组，递归展开。
fn load_symbols(path: &Path) -> ToolTab {
    let mut groups: Vec<ToolGroup> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) {
            let labels = parse_vhelp(doc.get("symbols").and_then(|s| s.get("vhelp")));
            if let Some(map) = doc.get("symbols").and_then(|s| s.as_mapping()) {
                for (k, v) in map {
                    let key = k.as_str().unwrap_or("");
                    if key == "vhelp" || key == "vlist" || key.is_empty() {
                        continue;
                    }
                    let mut items = Vec::new();
                    collect_yaml_strings(v, &mut items);
                    if items.is_empty() {
                        continue;
                    }
                    let label = labels
                        .get(key)
                        .cloned()
                        .unwrap_or_else(|| key.to_string());
                    groups.push(ToolGroup { label, items });
                }
            }
        }
    }
    ToolTab {
        name: "符号".into(),
        groups,
    }
}

/// 符号分类（顺序与产品参考图一致）。分类归属先按键名兜例外，再按 yaml 的
/// 分节注释（形如「#符号、电脑」）归类 —— 266 个键名逐个枚举太脆，而注释里
/// 又混着跨类别的条目（如「结构、偏旁、康熙（部首）、笔画、标点」同时含
/// 汉字部件与标点），所以两者结合。
pub const SYMBOL_CAT_NAMES: [&str; 10] = [
    "特殊符号",
    "标点符号",
    "数字序号",
    "数学/单位",
    "希腊/拉丁",
    "拼音/注音",
    "中文字符",
    "日文字符",
    "韩文字符",
    "俄文字母",
];

const CAT_SPECIAL: usize = 0;
const CAT_PUNCT: usize = 1;
const CAT_NUM: usize = 2;
const CAT_MATH: usize = 3;
const CAT_LATIN: usize = 4;
const CAT_PINYIN: usize = 5;
const CAT_HAN: usize = 6;
const CAT_JA: usize = 7;
const CAT_KO: usize = 8;
const CAT_RU: usize = 9;

/// 单个分类：名称 + 字符列表。
/// base 是这批字符在 emoji 雪碧图里的起始下标（表情分类用；符号分类为 None，
/// 它们靠字符查表）。有了 base，分类页不必逐字符查哈希，直接偏移取图。
pub struct ToolCat {
    pub name: String,
    pub items: Vec<String>,
    pub base: Option<usize>,
}

fn symbol_cat_of(section: &str, key: &str) -> Option<usize> {
    if key == "vhelp" || key == "vlist" {
        return None; // 索引表，不是内容
    }
    // 例外优先：这些键所属注释里混了别的类别
    match key {
        "vbd" | "vbdz" | "vbdzy" | "vdh" | "vjz" | "vfjz" | "vhj" => return Some(CAT_PUNCT),
        "vjg" | "vpp" | "vkx" | "vbh" | "vhzq" | "vhzh"
        | "vtg" | "vdz" | "vgz" | "vjq" => return Some(CAT_HAN),
        "vszq" | "vszh" | "vszd" | "vlm" | "vlmd" | "vfs" | "vszm"
        | "vyf" | "vrq" | "vyr" | "vsj" => return Some(CAT_NUM),
        "vsx" | "vdw" | "vhb" => return Some(CAT_MATH),
        "vxl" | "vxld" | "vzmq" | "vzmh" | "vsb" | "vxb" => return Some(CAT_LATIN),
        "vpy" | "vpyd" | "vzy" | "vsd" => return Some(CAT_PINYIN),
        "vey" | "veyd" => return Some(CAT_RU),
        "vhw" | "vhwq" | "vhwh" => return Some(CAT_KO),
        "viro" => return Some(CAT_JA),
        _ => {}
    }
    // v0..v10 是数字变体
    if key.len() > 1 && key.starts_with('v') && key[1..].chars().all(|c| c.is_ascii_digit()) {
        return Some(CAT_NUM);
    }
    Some(match section {
        "拉丁" => CAT_LATIN,
        "希腊" | "字母+圈v弧" | "上标、下标" => CAT_LATIN,
        "拼音、注音、声调" => CAT_PINYIN,
        "单位" | "货币" | "数学" => CAT_MATH,
        "数字+圆v弧v点" | "数字、分数" | "罗马数字" | "苏州码"
        | "月份、日期、曜日等" | "时间" => CAT_NUM,
        "结构、偏旁、康熙（部首）、笔画、标点" => CAT_HAN,
        "天干、地支、干支" | "节气" => CAT_HAN,
        "中英标点" | "将点号、非夹注、夹注、行间四个类别放在不同的列表中" => CAT_PUNCT,
        "假名" | "假名+圈" | "假名+半角" | "いろは順" => CAT_JA,
        "韩文" | "韩文+圈v弧" => CAT_KO,
        "俄语" => CAT_RU,
        _ => CAT_SPECIAL,
    })
}
/// 表情的全部字符（扁平化），优先用雪碧图元数据 —— 避免为了拿列表去解析
/// symbols_v.yaml（那是符号页的事，会被 load_symbol_cats 再解析一次）。
pub fn emoji_all_items(shared_dir: &Path) -> Vec<String> {
    if let Some(meta) = sheet_meta(shared_dir) {
        if !meta.groups.is_empty() {
            let mut out = Vec::with_capacity(meta.count);
            for g in meta.groups {
                out.extend(g.items);
            }
            return out;
        }
    }
    load_emoji(shared_dir).flat_items()
}

/// 表情分组：直接复用雪碧图元数据里的分组（顺序与位图下标一致）。
/// 每组是一个连续区间，所以只需记下起始下标即可精确定位位图。
pub fn emoji_cats(shared_dir: &Path) -> Vec<ToolCat> {
    let Some(meta) = sheet_meta(shared_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut base = 0usize;
    for g in &meta.groups {
        if g.items.is_empty() {
            continue;
        }
        out.push(ToolCat {
            name: g.label.clone(),
            items: g.items.clone(),
            base: Some(base),
        });
        base += g.items.len();
    }
    out
}

/// 解析 symbols_v.yaml 的分类视图。行扫描取「分节注释 + 键」的顺序，
/// serde 取每个键的内容；两者用键名对齐，保证顺序与显示稳定。
pub fn load_symbol_cats(path: &Path) -> Vec<ToolCat> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return Vec::new();
    };
    let Some(roots) = doc.get("symbols") else {
        return Vec::new();
    };

    // 行扫描：记录每个键上方的分节注释
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('#') && !t.starts_with("#!") {
            cur = t.trim_start_matches('#').trim().to_string();
            continue;
        }
        // 形如 "  'vfh': [ ... ]"
        if let Some(rest) = t.strip_prefix('\'') {
            if let Some((key, tail)) = rest.split_once('\'') {
                if tail.contains(':') {
                    sections.push((cur.clone(), key.to_string()));
                }
            }
        }
    }

    // 先按 (分类, 键) 分桶：同分类内部还要排「谁先显示」
    let mut buckets: Vec<Vec<(String, Vec<String>)>> = vec![Vec::new(); SYMBOL_CAT_NAMES.len()];
    for (section, key) in sections {
        let Some(cat) = symbol_cat_of(&section, &key) else { continue };
        let Some(v) = roots.get(key.as_str()) else { continue };
        let mut items = Vec::new();
        collect_yaml_strings(v, &mut items);
        items.retain(|i| !i.trim().is_empty());
        if !items.is_empty() {
            buckets[cat].push((key, items));
        }
    }

    let mut cats: Vec<ToolCat> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (idx, name) in SYMBOL_CAT_NAMES.iter().enumerate() {
        // 稳定排序：优先表里的键按表序靠前，其余保持 yaml 顺序
        let (head, tail) = cat_key_order(idx);
        // 头表优先、尾表最后，其余保持 yaml 原序（sort_by_key 稳定）
        buckets[idx].sort_by_key(|(k, _)| {
            if let Some(i) = head.iter().position(|p| p == k) {
                i
            } else if let Some(i) = tail.iter().position(|p| p == k) {
                10_000 + i
            } else {
                1_000
            }
        });
        let mut items = Vec::new();
        for (_, group) in &buckets[idx] {
            for item in group {
                if seen.insert(item.clone()) {
                    items.push(item.clone());
                }
            }
        }
        if !items.is_empty() {
            cats.push(ToolCat { name: name.to_string(), items, base: None });
        }
    }
    cats
}

/// 分类内部的小组顺序：(最前, 最后)。未列出的键保持 yaml 原序排在中间。
fn cat_key_order(cat: usize) -> (&'static [&'static str], &'static [&'static str]) {
    match cat {
        CAT_HAN => (
            &[
                "vjg", "vpp", "vkx", "vbh", "vhzq", "vhzh", "vtg", "vdz", "vgz", "vjq",
            ],
            &[],
        ),
        CAT_PUNCT => (&["vbd", "vbdz", "vbdzy", "vdh", "vjz", "vfjz", "vhj"], &[]),
        CAT_NUM => (&["vszq", "vszh", "vszd", "vlm", "vlmd", "vfs", "vszm"], &[]),
        // 希腊字母最常用，放在拉丁变体前面；圈字母/上下标排到最后
        CAT_LATIN => (&["vxl", "vxld"], &["vzmq", "vzmh", "vsb", "vxb"]),
        _ => (&[], &[]),
    }
}
/// `vhelp` 形如 `["va-Z(字母变体)","vfh(符号)",...]` → key → 中文名。
fn parse_vhelp(v: Option<&serde_yaml::Value>) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    if let Some(seq) = v.and_then(|x| x.as_sequence()) {
        for item in seq {
            let Some(s) = item.as_str() else { continue };
            // "vfh(符号)" → key="vfh" label="符号"；"va-Z(字母变体)" → key="va-Z"
            if let Some((key, rest)) = s.split_once('(') {
                if let Some(label) = rest.strip_suffix(')') {
                    out.insert(key.to_string(), label.to_string());
                }
            }
        }
    }
    out
}

fn collect_yaml_strings(v: &serde_yaml::Value, out: &mut Vec<String>) {
    match v {
        serde_yaml::Value::String(s) => {
            if !s.is_empty() {
                out.push(s.clone());
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            for x in seq {
                collect_yaml_strings(x, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 共享数据目录。该目录由 third_party 数据部署生成、不入版本库，CI 只跑
    /// 单元测试不部署共享数据，因此目录或符号表缺失时返回 None，
    /// 依赖真实数据的用例据此跳过而不是断言失败。
    fn shared_dir() -> Option<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("runtime/rime-shared");
        dir.join("symbols_v.yaml").is_file().then_some(dir)
    }

    #[test]
    fn emoji_group_is_stable() {
        assert_eq!(emoji_group(0x1F600), "笑脸与情感");
        assert_eq!(emoji_group(0x1F1E6), "旗帜");
        assert_eq!(emoji_group(0x2600), "符号与旗帜");
        assert_eq!(emoji_group(0x41), "其他");
    }

    #[test]
    fn flat_items_and_page_size() {
        let tab = ToolTab {
            name: "x".into(),
            groups: vec![
                ToolGroup { label: "a".into(), items: vec!["1".into(), "2".into()] },
                ToolGroup { label: "b".into(), items: vec!["3".into()] },
            ],
        };
        assert_eq!(tab.flat_items(), vec!["1", "2", "3"]);
        assert_eq!(VIEW_CELLS, 60);
    }

    #[test]
    fn parse_vhelp_entries() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            "symbols:\n  vhelp: ['va-Z(字母变体)', 'vfh(符号)']\n  vfh: [©, ®]\n",
        )
        .unwrap();
        let labels = parse_vhelp(v.get("symbols").and_then(|s| s.get("vhelp")));
        assert_eq!(labels.get("vfh").unwrap(), "符号");
        let mut items = Vec::new();
        collect_yaml_strings(v.get("symbols").unwrap().get("vfh").unwrap(), &mut items);
        assert_eq!(items, vec!["©", "®"]);
    }

    #[test]
    fn real_data_loads() {
        let Some(dir) = shared_dir() else { return };
        let tabs = load_tools(&dir);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].name, "表情");
        assert_eq!(tabs[1].name, "符号");
        assert!(tabs[0].flat_items().len() > 100, "表情应有去重后的实质数据");
        assert!(tabs[1].groups.len() > 10, "符号分组应解析出来");
        assert!(tabs[1].groups.iter().any(|g| g.label == "符号"), "vhelp 中文名应生效");
    }

    /// 含义表：优先 CLDR 官方中文名，缺失的回退 emoji.txt 触发词
    #[test]
    fn emoji_meanings_load() {
        let Some(dir) = shared_dir() else { return };
        let m = load_emoji_meanings(&dir);
        assert!(m.len() > 1000, "含义条数应覆盖大部分表情，实际 {}", m.len());
        // CLDR 短名（生成时人工覆盖了少数直译）
        assert_eq!(meaning_of(&m, "😩").map(|s| s.as_str()), Some("疲惫"));
        assert_eq!(meaning_of(&m, "😒").map(|s| s.as_str()), Some("不高兴"));
        assert_eq!(meaning_of(&m, "🍎").map(|s| s.as_str()), Some("红苹果"));
        // 带变体选择符的写法要能命中（⚠️ 与 ⚠ 两种写法）
        assert!(meaning_of(&m, "⚠️").is_some(), "⚠️ 应能查到");
        // ZWJ 组合走 emoji.txt 兜底
        assert!(meaning_of(&m, "👩‍❤️‍👨").is_some(), "ZWJ 序列应有兜底");
    }

    /// 分类覆盖：10 个分类都有内容，且总量与 yaml 非索引条目一致。
    #[test]
    fn symbol_cats_cover_all_groups() {
        let Some(dir) = shared_dir() else { return };
        let cats = load_symbol_cats(&dir.join("symbols_v.yaml"));
        let total: usize = cats.iter().map(|c| c.items.len()).sum();
        for c in &cats {
            println!("{:<10} {:5}", c.name, c.items.len());
        }
        println!("total {total}");
        assert_eq!(cats.len(), 10, "10 个分类都应非空");
        // 跨组去重后约 3967（原始 4826 条里有大量重复字符）
        assert!(total > 3500, "总量不应少于 3500，实际 {total}");
        for c in &cats {
            assert!(!c.items.is_empty(), "{} 为空", c.name);
        }
    }
    /// 雪碧图元数据与解码结果一致，且 flat_items 下标能命中位图。
    #[test]
    fn emoji_sheet_decodes_and_matches() {
        let Some(dir) = shared_dir() else { return };
        let Some(sheet) = load_emoji_sheet(&dir) else {
            return; // 资源未生成时跳过（部署形态允许）
        };
        let tabs = load_tools(&dir);
        assert_eq!(tabs[0].flat_items().len(), sheet.count,
            "flat_items 数量必须与雪碧图 count 一致，否则下标会错位");
        assert_eq!(sheet.pixels.len(), sheet.w * sheet.h * 4);
        assert!(sheet.origin(0).is_some());
        assert!(sheet.origin(sheet.count - 1).is_some());
        assert!(sheet.origin(sheet.count).is_none());
        // 表情 tab 按页下标能命中；符号字符按内容查不到
        let first = &tabs[0].flat_items()[0];
        assert_eq!(sheet.lookup(first), Some(0), "按字符查应命中第 0 格");
        assert_eq!(sheet.index_of_flat(7), Some(7));
        assert_eq!(sheet.lookup("ā"), None, "拼音字母不在表情雪碧图里 → 走文字");
        assert!(sheet.lookup("©").is_some(), "© 在 emoji.txt 里，有位图");
    }

    #[test]
    fn recent_store_dedup_order_and_persist() {
        let dir = std::env::temp_dir().join(format!("heng-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = RecentStore::new(&dir, RecentKind::Emoji);
        assert!(r.is_empty());
        for s in ["😩", "©", "😪", "😩", "♂"] {
            r.push(s);
        }
        // 最新在前 + 去重
        assert_eq!(r.tab().flat_items(), vec!["♂", "😩", "😪", "©"]);
        // 重新加载验证落盘
        let r2 = RecentStore::new(&dir, RecentKind::Emoji);
        assert_eq!(r2.tab().flat_items(), vec!["♂", "😩", "😪", "©"]);
        // 上限截断
        for i in 0..RECENT_MAX + 5 {
            r.push(&format!("x{i}"));
        }
        assert_eq!(r.tab().flat_items().len(), RECENT_MAX);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
