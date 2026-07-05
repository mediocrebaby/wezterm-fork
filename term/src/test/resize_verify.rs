//! Resize 校验框架 —— L1 模型层
//!
//! 目标：让「窗口尺寸变换后旧内容出现异样」这个问题变得可感知、可归因。
//! 这里只检验 wezterm 自身的模型逻辑（Screen::resize / rewrap_lines /
//! TerminalState::resize），不涉及真实 ConPTY 的重绘竞争（那是 L2
//! resize-probe 的职责）。
//!
//! 约定：
//! - 正常（非 #[ignore]）测试描述当前应当成立的不变量，跑红 = 你的改动
//!   在模型层引入了回归；
//! - `#[ignore = "known-issue: ..."]` 的测试描述**期望达到但当前尚未达到**
//!   的行为（特征测试）。用 `cargo test -p wezterm-term resize_verify --
//!   --ignored` 单独运行；某个 ignored 测试转绿说明对应已知问题被修复，
//!   此时应移除 ignore 标记将其纳入常规回归。

use super::*;

/// 不变量违规的分类，方便在失败输出里直接看到症状类型。
const V_LOST: &str = "丢失";
const V_DUP: &str = "重复";
const V_ORDER: &str = "错序";
const V_SPLIT: &str = "断裂(逻辑行未正确拼接)";

fn size(rows: usize, cols: usize) -> TerminalSize {
    TerminalSize {
        rows,
        cols,
        pixel_width: cols * 8,
        pixel_height: rows * 16,
        dpi: 0,
    }
}

fn resize_to(term: &mut TestTerm, rows: usize, cols: usize) {
    term.resize(size(rows, cols));
}

/// 把整个缓冲（scrollback + 视口）的物理行按 wrapped 标记拼回逻辑行。
/// rewrap 若丢失/错置 wrapped 标记，逻辑行就拼不回去，会被不变量检查捕获。
fn logical_lines(term: &Terminal) -> Vec<String> {
    let screen = term.screen();
    let mut out: Vec<String> = vec![];
    let mut pending: Option<String> = None;
    screen.for_each_phys_line(|_, line| {
        let text = line.as_str().to_string();
        let joined = match pending.take() {
            Some(mut p) => {
                p.push_str(&text);
                p
            }
            None => text,
        };
        if line.last_cell_was_wrapped() {
            pending = Some(joined);
        } else {
            out.push(joined);
        }
    });
    if let Some(p) = pending {
        out.push(p);
    }
    out
}

/// 核心不变量：resize 前打印过的每个 payload，在 resize 后必须
/// 恰好出现一次、完整（未被从中截断）、且相对顺序不变。
/// 返回违规列表；空列表 = 通过。
///
/// conpty 模式的例外：模拟 conhost 的「仅视口内 reflow」会在视口边界
/// 处断开 wrapped 标记（conhost 的缓冲里不存在 scrollback，Windows
/// Terminal 同理），逻辑行在边界拼不回来是该语义的预期行为。因此
/// 「断裂」在报警前先用物理行直接连接的文本复核内容完整性——内容
/// 仍连续存在则不算违规。
fn payload_violations(term: &Terminal, payloads: &[String]) -> Vec<String> {
    let logical = logical_lines(term);
    let mut violations = vec![];
    let mut last_pos: Option<usize> = None;

    // 物理行去尾随空白后直接连接（跨 wrap 段的 payload 在此连续）
    let phys_joined: String = {
        let mut s = String::new();
        term.screen().for_each_phys_line(|_, line| {
            s.push_str(line.as_str().trim_end());
        });
        s
    };

    for p in payloads {
        let hits: Vec<usize> = logical
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(p.as_str()))
            .map(|(i, _)| i)
            .collect();
        match hits.len() {
            0 => {
                // 区分「完全消失」与「还在但被拆断」：任一物理行含 payload
                // 的前 8 个字符即认为内容仍在、只是逻辑行没拼回来。
                let head: String = p.chars().take(8).collect();
                if phys_joined.contains(p.as_str()) {
                    // 内容完整且连续，仅 wrapped 标记在视口边界断开
                    // （conpty 语义预期），不算违规。
                } else if logical.iter().any(|l| l.contains(head.as_str())) {
                    violations.push(format!("{V_SPLIT}: {p}"));
                } else {
                    violations.push(format!("{V_LOST}: {p}"));
                }
            }
            1 => {
                if let Some(prev) = last_pos {
                    if hits[0] < prev {
                        violations.push(format!(
                            "{V_ORDER}: {p} 在逻辑行 {}，先前 payload 在 {}",
                            hits[0], prev
                        ));
                    }
                }
                last_pos = Some(hits[0]);
            }
            n => violations.push(format!("{V_DUP} {n} 次: {p}")),
        }
    }
    violations
}

fn assert_invariants(term: &Terminal, payloads: &[String], stage: &str) {
    let violations = payload_violations(term, payloads);
    if !violations.is_empty() {
        println!("--- 阶段 [{stage}] 逻辑行 dump ---");
        for (i, l) in logical_lines(term).iter().enumerate() {
            println!("{i:3}: [{l}]");
        }
        panic!(
            "阶段 [{stage}] 不变量违规 {} 项:\n  {}",
            violations.len(),
            violations.join("\n  ")
        );
    }
}

fn assert_cursor_in_bounds(term: &Terminal, stage: &str) {
    let cursor = term.cursor_pos();
    let screen = term.screen();
    // rewrap 的列 0 特例允许 cursor.x == cols（技术上越界一格）
    assert!(
        cursor.x <= screen.physical_cols,
        "阶段 [{stage}] cursor.x={} 超出 cols={}",
        cursor.x,
        screen.physical_cols
    );
    assert!(
        cursor.y >= 0 && cursor.y < screen.physical_rows as i64,
        "阶段 [{stage}] cursor.y={} 超出 rows={}",
        cursor.y,
        screen.physical_rows
    );
}

/// 生成 fixture：编号短行 + 强制换行的长 ASCII 行 + 长中文（宽字符）行。
/// 返回 (打印的字节, 校验用 payload 列表)。
fn build_fixture() -> (String, Vec<String>) {
    let mut text = String::new();
    let mut payloads = vec![];

    for i in 1..=6 {
        let p = format!("L{i:02}|short-{i:02}");
        text.push_str(&p);
        text.push_str("\r\n");
        payloads.push(p);
    }
    // 长 ASCII 行：在 80 列下也必然软换行
    let long_ascii: String = (0..12).map(|i| format!("SEG{i:02}-abcdefghij.")).collect();
    let p = format!("L07|{long_ascii}");
    text.push_str(&p);
    text.push_str("\r\n");
    payloads.push(p);
    // 长中文行：宽字符 + 软换行
    let long_cjk: String = "中文宽字符换行校验一二三四五六七八九十".repeat(4);
    let p = format!("L08|{long_cjk}");
    text.push_str(&p);
    text.push_str("\r\n");
    payloads.push(p);
    // 收尾标记行（用于视口检查）
    let p = "L09|TAIL-MARKER".to_string();
    text.push_str(&p);
    text.push_str("\r\n");
    payloads.push(p);

    (text, payloads)
}

/// 跑一个标准的 resize 序列，每步之后检查不变量。
fn run_resize_sequence(term: &mut TestTerm, payloads: &[String]) {
    for (rows, cols, stage) in [
        (24, 40, "缩窄 80->40"),
        (24, 120, "放宽 40->120"),
        (10, 120, "变矮 24->10"),
        (30, 120, "变高 10->30"),
        (24, 80, "回到 80x24"),
    ] {
        resize_to(term, rows, cols);
        assert_invariants(term, payloads, stage);
        assert_cursor_in_bounds(term, stage);
    }
}

/// 基线：非 ConPTY 路径（unix / ssh 直连），rewrap 后内容不丢、不重、不乱序。
#[test]
fn resize_verify_roundtrip_plain() {
    let mut term = TestTerm::new(24, 80, 1000);
    let (text, payloads) = build_fixture();
    term.print(text);
    assert_invariants(&term, &payloads, "初始");
    run_resize_sequence(&mut term, &payloads);
}

/// ConPTY quirks 路径：同样的不变量。
/// 注意：这里没有真实 ConPTY 的重绘流量，只检验 wezterm 自身在
/// is_conpty=true 时的 resize 行为（resize_preserves_scrollback 分支）。
#[test]
fn resize_verify_roundtrip_conpty_quirks() {
    let mut term = TestTerm::new(24, 80, 1000);
    term.enable_conpty_quirks();
    let (text, payloads) = build_fixture();
    term.print(text);
    assert_invariants(&term, &payloads, "初始");
    run_resize_sequence(&mut term, &payloads);
}

/// 宽字符（中文）在窄宽往返后必须保持完整，不允许出现半个字符
/// 或被截断的情况。
#[test]
fn resize_verify_cjk_narrow_widen() {
    let mut term = TestTerm::new(10, 20, 100);
    let cjk = "中文测试甲乙丙丁戊己庚辛壬癸".to_string();
    term.print(format!("{cjk}\r\n"));
    let payloads = vec![cjk];
    for (rows, cols, stage) in [
        (10, 11, "缩窄到 11(奇数列,宽字符边界)"),
        (10, 40, "放宽到 40"),
        (10, 20, "回到 20"),
    ] {
        resize_to(&mut term, rows, cols);
        assert_invariants(&term, &payloads, stage);
    }
}

/// 曾是【known-issue ③】：performer.rs 的 makes_sense_to_wrap 启发式
/// 使 ConPTY 模式下空格处软换行的行不标 wrapped，放宽后拼不回
/// （lsd/ls 网格输出重灾区），且与 conhost 的 wrap 语义不一致导致
/// reflow 失步。2026-07-05 移除该启发式后转绿，摘除 ignore 纳入回归。
#[test]
fn resize_verify_conpty_space_at_wrap_boundary() {
    let mut term = TestTerm::new(8, 5, 100);
    term.enable_conpty_quirks();
    // 宽度 5："abcd efgh" 在第 5 列(空格)处软换行
    term.print("abcd efgh\r\n");
    resize_to(&mut term, 8, 20);
    // 期望：放宽后拼回一行（与非 conpty 行为一致）
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["abcd efgh", "", "", "", "", "", "", ""],
    );
}

/// 对照组：同样场景非 ConPTY 路径当前即为期望行为。
#[test]
fn resize_verify_plain_space_at_wrap_boundary() {
    let mut term = TestTerm::new(8, 5, 100);
    term.print("abcd efgh\r\n");
    resize_to(&mut term, 8, 20);
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["abcd efgh", "", "", "", "", "", "", ""],
    );
}

/// conpty quirks 开启时变高后尾部内容仍在视口内。
/// 基线验证表明纯模型层此场景是好的 —— 问题②（垫空行/需回滚）只有在
/// 真实 ConPTY 重绘流量参与时才显现，由 L2 resize-probe 负责捕获。
/// 本测试留作回归守卫：若你的改动让它变红，说明模型层被改坏了。
#[test]
fn resize_verify_conpty_tail_visible_after_grow() {
    let mut term = TestTerm::new(10, 40, 1000);
    term.enable_conpty_quirks();
    for i in 1..=20 {
        term.print(format!("L{i:02}|line\r\n"));
    }
    // 变高：期望能看到更多历史，且尾部内容不离开视口
    resize_to(&mut term, 20, 40);
    let visible: Vec<String> = term
        .screen()
        .visible_lines()
        .iter()
        .map(|l| l.as_str().to_string())
        .collect();
    assert!(
        visible.iter().any(|l| l.contains("L20|line")),
        "变高后尾部行 L20 应该仍在视口内，实际视口:\n{:#?}",
        visible
    );
}

/// 对照组：非 ConPTY 路径变高后尾部内容在视口内（mutable scrollback，
/// bottom gravity）。
#[test]
fn resize_verify_plain_tail_visible_after_grow() {
    let mut term = TestTerm::new(10, 40, 1000);
    for i in 1..=20 {
        term.print(format!("L{i:02}|line\r\n"));
    }
    resize_to(&mut term, 20, 40);
    let visible: Vec<String> = term
        .screen()
        .visible_lines()
        .iter()
        .map(|l| l.as_str().to_string())
        .collect();
    assert!(
        visible.iter().any(|l| l.contains("L20|line")),
        "变高后尾部行 L20 应该仍在视口内，实际视口:\n{:#?}",
        visible
    );
}

/// 模拟 L2 风暴：输出与连续 resize **交错**。
/// L2 的 raw-stream.bin 证明 ConPTY 全程只转发纯线性流（TICK-NNN\r\n，
/// 无任何重绘/定位序列），因此 L2 风暴检出的错序/拼行理应在纯模型层复现。
/// 与 run_resize_sequence 的区别：resize 之间穿插 print（拖拽时 child
/// 仍在输出），部分 resize 步之间无输出（贴近 30ms 步长 vs 300ms TICK
/// 的时序比例）。
fn run_interleaved_storm(term: &mut TestTerm) -> Vec<String> {
    let (text, mut payloads) = build_fixture();
    term.print(text);

    let mut tick = 0usize;
    macro_rules! emit_tick {
        () => {{
            let p = format!("TICK-{:03}", tick);
            term.print(format!("{p}\r\n"));
            payloads.push(p);
            tick += 1;
        }};
    }

    // 先重演 L2 的 5 个单步阶段（阶段间有 TICK 流量）
    for (rows, cols) in [(24, 40), (24, 120), (10, 120), (30, 120), (24, 80)] {
        emit_tick!();
        emit_tick!();
        resize_to(term, rows, cols);
        emit_tick!();
    }
    // 拖拽风暴 3 轮：约每 3 步 resize 才有一行输出
    let storm_cols = [78, 74, 68, 62, 55, 48, 44, 40, 48, 60, 72, 80];
    for _round in 0..3 {
        for (i, cols) in storm_cols.iter().enumerate() {
            if i % 3 == 1 {
                emit_tick!();
            }
            resize_to(term, 24, *cols);
        }
        emit_tick!();
        emit_tick!();
    }
    payloads
}

/// ConPTY quirks 下「交错输出 × 连续 resize」回归守卫。
/// 曾是 known-issue：rewrap_lines 的列 0 特例（screen.rs）把停在
/// 空行 x=0 的光标误拉回上一行，后续线性输出覆盖旧内容 —— 即用户
/// 所见「resize 后旧内容异样」（丢失/拼行/错序三类症状同源）。
/// 2026-07-05 修复（特例增加 num_lines > 0 条件）后转绿，摘除 ignore
/// 纳入常规回归。
#[test]
fn resize_verify_interleaved_storm_conpty() {
    let mut term = TestTerm::new(24, 80, 1000);
    term.enable_conpty_quirks();
    let payloads = run_interleaved_storm(&mut term);
    assert_invariants(&term, &payloads, "conpty 交错风暴");
    assert_cursor_in_bounds(&term, "conpty 交错风暴");
}

/// 对照组：非 ConPTY 路径同样的交错风暴。
#[test]
fn resize_verify_interleaved_storm_plain() {
    let mut term = TestTerm::new(24, 80, 1000);
    let payloads = run_interleaved_storm(&mut term);
    assert_invariants(&term, &payloads, "plain 交错风暴");
    assert_cursor_in_bounds(&term, "plain 交错风暴");
}

/// 最小回归守卫：流式输出（\r\n 结尾）后光标停在独立空行的 x=0，
/// 缩窄 resize 的 rewrap 不得把光标拉回上一行（rewrap_lines 列 0
/// 特例的误伤场景）。若回归，resize 之后的第一行输出会原地覆盖
/// 最后一条已有输出。
#[test]
fn resize_verify_cursor_blank_line_not_pulled_up() {
    let mut term = TestTerm::new(24, 80, 1000);
    let (text, mut payloads) = build_fixture();
    term.print(text);
    term.print("TICK-000\r\n");
    term.print("TICK-001\r\n");
    payloads.push("TICK-000".to_string());
    payloads.push("TICK-001".to_string());
    // 缩窄使 fixture 长行多占物理行，光标折算必须跟随位移
    resize_to(&mut term, 24, 40);
    // 若光标被误拉回上一行，这次输出会覆盖 TICK-001
    term.print("TICK-002\r\n");
    payloads.push("TICK-002".to_string());
    assert_invariants(&term, &payloads, "缩窄后继续输出");
}

/// 输入场景：光标停在 prompt 之后的行中间（x>0，行尾是空格），
/// resize 与逐字符输入回显交错 —— 模拟用户在提示符处拖拽窗口后
/// 继续敲命令。检查 prompt+输入行不断裂、不重复，历史输出完好。
fn run_input_interleaved(term: &mut TestTerm, conpty: bool) {
    if conpty {
        term.enable_conpty_quirks();
    }
    let (text, mut payloads) = build_fixture();
    term.print(text);

    // prompt 行尾带空格（known-issue ③ 的敏感形态），光标停在 x=9
    term.print("PS TEST> ");
    for (i, (rows, cols)) in [(24, 60), (24, 40), (24, 30), (24, 47), (24, 80)]
        .iter()
        .enumerate()
    {
        resize_to(term, *rows, *cols);
        // 每次 resize 后回显几个输入字符
        term.print(format!("in{i}-"));
    }
    term.print("done");
    // 回车执行：输入行成为历史，输出结果
    term.print("\r\n");
    term.print("CMD-OUTPUT-OK\r\n");

    payloads.push("PS TEST> in0-in1-in2-in3-in4-done".to_string());
    payloads.push("CMD-OUTPUT-OK".to_string());
    let tag = if conpty { "conpty" } else { "plain" };
    assert_invariants(term, &payloads, &format!("{tag} 输入交错"));
    assert_cursor_in_bounds(term, &format!("{tag} 输入交错"));
}

#[test]
fn resize_verify_input_interleaved_conpty() {
    let mut term = TestTerm::new(24, 80, 1000);
    run_input_interleaved(&mut term, true);
}

#[test]
fn resize_verify_input_interleaved_plain() {
    let mut term = TestTerm::new(24, 80, 1000);
    run_input_interleaved(&mut term, false);
}

/// 用户实测形态复现（lsd 网格 + 多跳拖拽）：含 Nerd Font PUA 图标的
/// 95 列宽行，在 conpty 模式下经历「每 2 列一跳」的连续缩窄再放宽
/// （max_fps=120 的 live-resize 事件形态，期间 ConPTY 零流量），
/// 每个输出行放宽后必须拼回单一逻辑行。
#[test]
fn resize_verify_conpty_lsd_grid_multistep() {
    let mut term = TestTerm::new(34, 100, 1000);
    term.enable_conpty_quirks();

    let rows = [
        "\u{f115} Code       \u{f024d} Downloads  \u{f115} Links         \u{f115} OneDrive           \u{f115} 'Saved Games'  \u{f115} VideoCaptioner",
        "\u{f115} Contacts   \u{f115} Favorites  \u{f1359} Music          package-lock.json  \u{f115} scoop          \u{f115} Videos",
        "\u{f108} Desktop    \u{f115} Intel      \u{e5fa} node_modules  \u{f024f} Pictures           \u{f115} Searches       \u{f115} vscode-remote-wsl",
        "\u{f02d} Documents  \u{f115} Library    \u{f115} npm-cache     \u{f115} sangfor            \u{f115} software       \u{f115} xwechat_files",
    ];
    term.print("PowerShell 7.6.3\r\n");
    term.print("\u{279c}  ~ ls\r\n");
    for r in &rows {
        term.print(format!("{r}\r\n"));
    }
    term.print("\u{279c}  ~ ");

    // 连续小跳缩窄再放宽（GUI max_fps=120 live-resize 不合并事件）。
    // 逐跳追踪第一个 lsd 行的可拼接性，定位丢失 wrapped 标记的跳。
    let mut check = |term: &Terminal, tag: &str| {
        let ok0 = logical_lines(term).iter().any(|l| l.contains(rows[0]));
        let ok1 = logical_lines(term).iter().any(|l| l.contains(rows[1]));
        println!("{tag}: row0_join={ok0} row1_join={ok1}");
        if !ok0 {
            term.screen().for_each_phys_line(|i, l| {
                if i >= 2 && i <= 4 {
                    println!(
                        "  {tag} 行{i} cells={} wrapped={} [{}]",
                        l.len(),
                        l.last_cell_was_wrapped(),
                        l.as_str()
                    );
                }
            });
        }
    };
    let mut cols = 98usize;
    while cols > 33 {
        resize_to(&mut term, 34, cols);
        check(&term, &format!("缩 cols={cols}"));
        cols -= 2;
    }
    resize_to(&mut term, 34, 33);
    check(&term, "缩 cols=33");
    let mut cols = 35usize;
    while cols < 100 {
        resize_to(&mut term, 34, cols);
        check(&term, &format!("放 cols={cols}"));
        cols += 2;
    }
    resize_to(&mut term, 34, 100);
    check(&term, "放 cols=100");

    // 每个 lsd 行必须拼回单一逻辑行
    let logical = logical_lines(&term);
    let mut violations = vec![];
    for r in &rows {
        if !logical.iter().any(|l| l.contains(r)) {
            violations.push(format!("未拼回单一逻辑行: {}", &r[..40.min(r.len())]));
        }
    }
    // 视口检查：内容尾部（最后一个 prompt）应在视口内且其上不应有
    // 大量空行把内容顶出视口
    let screen = term.screen();
    let visible: Vec<String> = screen
        .visible_lines()
        .iter()
        .map(|l| l.as_str().to_string())
        .collect();
    if !visible.iter().any(|l| l.contains("Documents")) {
        violations.push("内容被顶出视口: Documents 行不在可见区".to_string());
    }
    if !violations.is_empty() {
        println!("--- 物理行 dump ---");
        screen.for_each_phys_line(|i, line| {
            println!(
                "{i:3} wrapped={} [{}]",
                line.last_cell_was_wrapped(),
                line.as_str()
            );
        });
        panic!("违规 {} 项:\n  {}", violations.len(), violations.join("\n  "));
    }
}

/// 用户「滚动后偶尔错位」复现：大量中等宽度行（ls -l 形态，60 字符），
/// 多跳缩放后 scrollback 深处的每一行都必须拼回单一逻辑行。
/// 症状（scroll1 截图）：恰好骑跨某跳视口边界的行丢失 wrapped 链，
/// 留下窄宽时期的断行（如 `Carg` + `o.toml`）。
#[test]
fn resize_verify_conpty_scrollback_lines_rejoin_after_multistep() {
    let mut term = TestTerm::new(36, 120, 2000);
    term.enable_conpty_quirks();

    let mut payloads = vec![];
    for i in 1..=100 {
        // 混合行宽：短行 / ls -l 形态 60 字符 / 长行 95 字符
        let p = match i % 3 {
            0 => format!("d----  2026/6/27  0:47  dir-{i:03}"),
            1 => format!(
                "-a---  2026/6/27  0:47  {:6}  file-{i:02}.sample-name.toml",
                i * 137
            ),
            _ => format!(
                "-a---  2026/6/27  0:47  {:8}  very-long-file-name-{i:03}.with.many.dotted.segments.and-suffix.extension",
                i * 991
            ),
        };
        term.print(format!("{p}\r\n"));
        payloads.push(p);
    }
    term.print("PS> ");

    // 步长 1 的逐列拖拽（最贴近 max_fps=120 的真实事件流）
    for cols in (33..=119).rev() {
        resize_to(&mut term, 36, cols);
    }
    for cols in 34..=120 {
        resize_to(&mut term, 36, cols);
    }

    let logical = logical_lines(&term);
    let mut violations = vec![];
    for p in &payloads {
        if !logical.iter().any(|l| l.contains(p.as_str())) {
            violations.push(format!("未拼回: {p}"));
        }
    }
    if !violations.is_empty() {
        println!("--- 物理行 dump（前 60 行）---");
        term.screen().for_each_phys_line(|i, line| {
            if i < 60 {
                println!(
                    "{i:3} wrapped={} [{}]",
                    line.last_cell_was_wrapped(),
                    line.as_str()
                );
            }
        });
        panic!(
            "违规 {} 项:\n  {}",
            violations.len(),
            violations.join("\n  ")
        );
    }
}

/// alt screen（全屏应用)路径：resize 不做 rewrap，只截断/失效。
/// 这里只锁定「不 panic、光标在界内」的最低保证。
#[test]
fn resize_verify_alt_screen_stability() {
    let mut term = TestTerm::new(24, 80, 100);
    term.print("primary content\r\n");
    // 进入 alt screen
    term.set_mode("?1049", true);
    term.print("alt screen content");
    for (rows, cols, stage) in [(10, 40, "alt 缩小"), (30, 120, "alt 放大")] {
        resize_to(&mut term, rows, cols);
        assert_cursor_in_bounds(&term, stage);
    }
    // 退出 alt screen 后主屏内容仍在
    term.set_mode("?1049", false);
    let payloads = vec!["primary content".to_string()];
    assert_invariants(&term, &payloads, "退出 alt screen");
}

/// 用户实测场景：nvim（alt screen）期间拖拽「缩窄→放宽」，退出 nvim 后
/// conhost 按它自己的主缓冲坐标系发 CUP —— 要求主屏在 alt 激活期间的
/// conpty reflow 不改变缓冲总行数（视口顶保持 = conhost 缓冲顶），否则
/// 退出后的所有输入回显都落错行（prompt 与输入分离）。
#[test]
fn resize_verify_conpty_alt_screen_resize_roundtrip() {
    let mut term = TestTerm::new(34, 100, 2000);
    term.enable_conpty_quirks();

    // 模拟 pwsh 主屏：14 行内容 + 「nvim .」prompt 行，回车后光标停在
    // row15（独立空行）—— 与 probe-nvim 抓到的真实布局一致。
    let mut payloads = vec![];
    for i in 0..13 {
        let p = format!("line-{i:02}-{}", "x".repeat(62));
        term.print(format!("{p}\r\n"));
        payloads.push(p);
    }
    term.print("\r\n"); // row13 空行
    term.print("PS C:\\Users\\JustSo\\Code\\wezterm-fork> nvim .\r\n"); // row14，光标到 row15
    payloads.push("PS C:\\Users\\JustSo\\Code\\wezterm-fork> nvim .".to_string());

    let lines_before = term.screen().scrollback_rows();
    assert!(lines_before == 34, "进入 alt 前缓冲应恰为视口高");

    // 进入 alt screen（1049 = 保存主屏光标 + 切换 + 清屏）
    term.set_mode("?1049", true);
    term.print("NVIM UI ROW\r\n".repeat(20));

    // alt 激活期间缩窄再放宽。窄到主屏内容 wrap 膨胀必然超过视口高
    // （27 列时 70 字符行占 3 段），这是真实 GUI 复现里触发失步的条件：
    // 若主屏每跳都 reflow，「溢出进 scrollback → 放宽拼回」回环有损，
    // 总行数回不到 34。conhost 对非活动主缓冲不做任何 reflow。
    for cols in (27..=99).rev().step_by(2) {
        resize_to(&mut term, 34, cols);
    }
    for cols in (29..=100).step_by(2) {
        resize_to(&mut term, 34, cols);
    }

    // 退出 alt screen，恢复主屏与保存的光标
    term.set_mode("?1049", false);

    // conhost 视角：主缓冲从未有 scrollback，缓冲顶=row1。退出后它在恢复
    // 的光标处顺序打印新 prompt，随后 PSReadLine 用 CUP(16;x) 重绘输入行。
    term.print("PS C:\\Users\\JustSo\\Code\\wezterm-fork> ");

    // 不变量 1：缓冲总行数回到 34 —— 视口顶 = conhost 缓冲顶（无多余
    // 垫行把视口顶推离 phys0）。
    let total = term.screen().scrollback_rows();
    if total != 34 {
        println!("--- 物理行 dump ---");
        term.screen().for_each_phys_line(|i, line| {
            println!("{i:3}: [{}]", line.as_str());
        });
    }
    assert!(total == 34,
        "alt 期间 resize 回环后主缓冲总行数应不变（conhost 无 scrollback）"
    );

    // 不变量 2：新 prompt 打印在 phys15（= conhost 的 1-based row16），
    // 后续 CUP(16;39) 才能落回 prompt 行。
    let prompt_phys: Vec<usize> = {
        let mut v = vec![];
        term.screen().for_each_phys_line(|i, line| {
            if line.as_str().starts_with("PS C:") && line.as_str().trim_end().ends_with('>') {
                v.push(i);
            }
        });
        v
    };
    assert!(
        prompt_phys == vec![15],
        "退出 alt 后新 prompt 应在 phys15（conhost CUP(16) 的落点），实际 {prompt_phys:?}"
    );

    // 不变量 3：CUP(16;39) 输入回显必须落在 prompt 行上
    term.print("\x1b[16;39Hecho MARKER");
    let mut ok = false;
    term.screen().for_each_phys_line(|i, line| {
        if i == 15 && line.as_str().contains("echo MARKER") {
            ok = true;
        }
    });
    assert!(ok, "CUP(16;39) 的输入回显必须与 prompt 同行（phys15）");

    // 不变量 4：主屏原有内容完好
    assert_invariants(&term, &payloads, "alt-resize 回环后主屏");
}

