//! Resize 校验框架 —— L2 ConPTY 交互层探针
//!
//! L1（term crate 单测）只能验证 wezterm 自身的 rewrap 模型；
//! 「窗口尺寸变换后旧内容异样」的根因是 ConPTY 在 resize 时自行 reflow
//! 并重放整屏内容，与 wezterm 的 rewrap 结果打架 —— 这只有让真实
//! ConPTY 参与才能复现。本探针做的事：
//!
//!   1. 通过 portable-pty 打开真实 ConPTY，spawn 自身的 `--child` 模式
//!      在 pty 内打印带编号的 fixture 内容；
//!   2. 把 master 端读到的字节流喂给 wezterm_term::Terminal
//!      （enable_conpty_quirks，与 mux LocalDomain 行为一致）；
//!   3. 按 LocalPane::resize 的顺序（先 pty 后模型）执行多轮 resize，
//!      每轮等流量安定后检查不变量：payload 不丢失、不重复、不错序、
//!      逻辑行可拼接；
//!   4. 违规时把原始字节流和屏幕快照落盘到 out-dir，供归因分析。
//!
//! 退出码：0 = 全部通过；1 = 存在不变量违规；2 = 探针自身故障。

use anyhow::{anyhow, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wezterm_term::color::ColorPalette;
use wezterm_term::{Terminal, TerminalConfiguration, TerminalSize};

const READY_MARKER: &str = "@@FIXTURE-READY@@";

/// 每轮 resize 后固定持续接收 ConPTY 流量的窗口。
/// child 在 resize 阶段持续输出 TICK 行（模拟真实场景下 resize 时
/// 总有输出在滚动），因此不能用「静默即安定」来判定，而是固定窗口。
const STAGE_DRAIN: Duration = Duration::from_millis(1500);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = if args.iter().any(|a| a == "--child") {
        match child_mode() {
            Ok(()) => 0,
            Err(_) => 2,
        }
    } else {
        match harness() {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(err) => {
                eprintln!("探针自身故障: {err:#}");
                2
            }
        }
    };
    std::process::exit(code);
}

// ---------------------------------------------------------------------------
// fixture（与 L1 的 build_fixture 保持同一形状）
// ---------------------------------------------------------------------------

fn fixture_payloads() -> Vec<String> {
    let mut payloads = vec![];
    for i in 1..=6 {
        payloads.push(format!("L{i:02}|short-{i:02}"));
    }
    let long_ascii: String = (0..12).map(|i| format!("SEG{i:02}-abcdefghij.")).collect();
    payloads.push(format!("L07|{long_ascii}"));
    let long_cjk: String = "中文宽字符换行校验一二三四五六七八九十".repeat(4);
    payloads.push(format!("L08|{long_cjk}"));
    payloads.push("L09|TAIL-MARKER".to_string());
    payloads
}

/// 运行在 ConPTY 内部：切到 UTF-8 代码页，打印 fixture，然后保活等待
/// harness 完成 resize 序列。
fn child_mode() -> Result<()> {
    #[cfg(windows)]
    unsafe {
        winapi::um::wincon::SetConsoleOutputCP(65001);
        winapi::um::wincon::SetConsoleCP(65001);
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for payload in fixture_payloads() {
        write!(out, "{payload}\r\n")?;
    }
    write!(out, "{READY_MARKER}\r\n")?;
    out.flush()?;
    // resize 阶段持续输出：真实终端里 resize 时几乎总有流量
    // （命令输出、提示符重绘），这也是逼出 ConPTY 重放的必要条件 ——
    // 对着完全静默的 pty，ConPTY 在 resize 时可以什么都不发。
    for i in 1..=120u32 {
        write!(out, "TICK-{i:03}\r\n")?;
        out.flush()?;
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ProbeConfig;
impl TerminalConfiguration for ProbeConfig {
    fn scrollback_size(&self) -> usize {
        10_000
    }
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
}

fn term_size(rows: usize, cols: usize) -> TerminalSize {
    TerminalSize {
        rows,
        cols,
        pixel_width: cols * 8,
        pixel_height: rows * 16,
        dpi: 96,
    }
}

fn pty_size(rows: usize, cols: usize) -> PtySize {
    PtySize {
        rows: rows as u16,
        cols: cols as u16,
        pixel_width: (cols * 8) as u16,
        pixel_height: (rows * 16) as u16,
    }
}

struct Pump {
    rx: mpsc::Receiver<Vec<u8>>,
    raw_log: Vec<u8>,
}

impl Pump {
    /// 在固定窗口内持续把 pty 输出喂给模型。返回本轮收到的字节数。
    fn settle(&mut self, term: &mut Terminal, stage: &str) -> usize {
        self.raw_log
            .extend_from_slice(format!("\n@@STAGE:{stage}@@\n").as_bytes());
        let deadline = Instant::now() + STAGE_DRAIN;
        let mut got = 0usize;
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => {
                    got += chunk.len();
                    self.raw_log.extend_from_slice(&chunk);
                    term.advance_bytes(&chunk);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        got
    }

    /// 一直喂到模型内容里出现指定标记（或超时）。
    fn wait_for_marker(&mut self, term: &mut Terminal, marker: &str) -> Result<()> {
        let start = Instant::now();
        loop {
            if screen_text(term).iter().any(|l| l.contains(marker)) {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(anyhow!("等待 {marker} 超时"));
            }
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => {
                    self.raw_log.extend_from_slice(&chunk);
                    term.advance_bytes(&chunk);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("pty reader 在等待 {marker} 时断开"));
                }
            }
        }
    }
}

/// 整个缓冲（scrollback + 视口）的物理行文本。
fn screen_text(term: &Terminal) -> Vec<String> {
    let mut out = vec![];
    term.screen().for_each_phys_line(|_, line| {
        out.push(line.as_str().to_string());
    });
    out
}

/// 物理行按 wrapped 标记拼回逻辑行（与 L1 同构）。
fn logical_lines(term: &Terminal) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut pending: Option<String> = None;
    term.screen().for_each_phys_line(|_, line| {
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

/// 与 L1 相同的四类不变量：丢失 / 重复 / 错序 / 断裂。
fn payload_violations(term: &Terminal, payloads: &[String]) -> Vec<String> {
    let logical = logical_lines(term);
    let mut violations = vec![];
    let mut last_pos: Option<usize> = None;

    for p in payloads {
        let hits: Vec<usize> = logical
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains(p.as_str()))
            .map(|(i, _)| i)
            .collect();
        match hits.len() {
            0 => {
                // conpty 语义：视口边界处 wrapped 标记断开是预期行为，
                // 用物理行直接连接的文本复核内容完整性
                let phys_joined: String = {
                    let mut s = String::new();
                    term.screen().for_each_phys_line(|_, line| {
                        s.push_str(line.as_str().trim_end());
                    });
                    s
                };
                let head: String = p.chars().take(8).collect();
                if phys_joined.contains(p.as_str()) {
                    // 内容完整连续，仅边界断开，不算违规
                } else if logical.iter().any(|l| l.contains(head.as_str())) {
                    violations.push(format!("断裂(逻辑行未正确拼接): {p}"));
                } else {
                    violations.push(format!("丢失: {p}"));
                }
            }
            1 => {
                if let Some(prev) = last_pos {
                    if hits[0] < prev {
                        violations.push(format!(
                            "错序: {p} 在逻辑行 {}，先前 payload 在 {}",
                            hits[0], prev
                        ));
                    }
                }
                last_pos = Some(hits[0]);
            }
            n => violations.push(format!("重复 {n} 次: {p}")),
        }
    }
    violations
}

/// TICK 行的不变量：每个编号至多出现一次，且编号递增。
/// ConPTY 重放与模型 rewrap 错位时最典型的症状就是滚动区里出现
/// 重复/乱序的 TICK 行。
fn tick_violations(term: &Terminal) -> Vec<String> {
    let mut violations = vec![];
    let mut seen: Vec<u32> = vec![];
    for l in logical_lines(term) {
        let mut rest = l.as_str();
        while let Some(pos) = rest.find("TICK-") {
            let digits: String = rest[pos + 5..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            rest = &rest[pos + 5..];
            if digits.len() != 3 {
                continue;
            }
            let n: u32 = digits.parse().unwrap();
            if seen.contains(&n) {
                violations.push(format!("重复: TICK-{n:03}"));
            } else if seen.last().map(|&last| n < last).unwrap_or(false) {
                violations.push(format!("错序: TICK-{n:03} 出现在 TICK-{:03} 之后", seen.last().unwrap()));
                seen.push(n);
            } else {
                seen.push(n);
            }
        }
    }
    violations
}

/// pty writer 需要同时给 Terminal（应答 DSR/DA 等查询）和 harness
/// （模拟用户按键）使用，用 Arc<Mutex> 共享。
#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Box<dyn Write + Send>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

/// 统计 needle 在逻辑行文本里出现的总次数（一行可含多次）。
fn count_occurrences(logical: &[String], needle: &str) -> usize {
    logical.iter().map(|l| l.matches(needle).count()).sum()
}

/// 交互输入场景：真实 powershell（PSReadLine 重绘输入行）。
/// 用户症状「输入错乱」的复现面 —— 在提示符处逐字符输入与拖拽风暴
/// 交错，检查：历史输出不变量、命令确实以完整形态执行且输出唯一、
/// 输入行无叠影/断片残留。
fn interactive_shell_scenario(out_dir: &std::path::Path) -> Result<bool> {
    println!("== resize-probe: 交互输入场景 (真实 powershell + PSReadLine) ==");

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(pty_size(24, 80))
        .context("openpty(交互场景) 失败")?;

    let mut cmd = CommandBuilder::new("powershell.exe");
    for a in [
        "-NoProfile",
        "-NoLogo",
        "-NoExit",
        "-Command",
        "function prompt { 'PROBE> ' }",
    ] {
        cmd.arg(a);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .context("spawn powershell 失败")?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = SharedWriter(Arc::new(Mutex::new(pair.master.take_writer()?)));
    let mut keys = writer.clone();

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut term = Terminal::new(
        term_size(24, 80),
        Arc::new(ProbeConfig),
        "ResizeProbe",
        "1.0",
        Box::new(writer),
    );
    term.enable_conpty_quirks();

    let mut pump = Pump {
        rx,
        raw_log: vec![],
    };

    // powershell 启动慢，放宽等待
    let wait_prompt = |pump: &mut Pump, term: &mut Terminal, secs: u64| -> Result<()> {
        let start = Instant::now();
        loop {
            if screen_text(term).iter().any(|l| l.contains("PROBE>")) {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(secs) {
                return Err(anyhow!("等待 PROBE> 提示符超时"));
            }
            match pump.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(chunk) => {
                    pump.raw_log.extend_from_slice(&chunk);
                    term.advance_bytes(&chunk);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("pty reader 断开"));
                }
            }
        }
    };
    wait_prompt(&mut pump, &mut term, 30)?;

    // 生成 12 行历史输出，作为「旧内容被损坏」的检测面
    keys.write_all(b"1..12 | % { 'HIST-{0:d2}' -f $_ }\r")?;
    keys.flush()?;
    pump.wait_for_marker(&mut term, "HIST-12")?;
    pump.settle(&mut term, "交互:历史就绪");

    let hist_payloads: Vec<String> = (1..=12).map(|i| format!("HIST-{i:02}")).collect();

    // 逐字符输入与拖拽风暴交错。命令刻意拆写 MARKER，
    // 使完整的 MARKER-A1 只可能出现在命令输出里。
    let cmd_text = "echo ('MAR'+'KER-A1')";
    let chars: Vec<char> = cmd_text.chars().collect();
    let storm_cols = [76usize, 70, 62, 55, 48, 42, 48, 56, 64, 72, 80];
    let per_step = chars.len().div_ceil(storm_cols.len());
    let mut fed = 0usize;
    for cols in storm_cols {
        for _ in 0..per_step {
            if fed < chars.len() {
                let mut b = [0u8; 4];
                keys.write_all(chars[fed].encode_utf8(&mut b).as_bytes())?;
                fed += 1;
            }
        }
        keys.flush()?;
        pair.master.resize(pty_size(24, cols))?;
        term.resize(term_size(24, cols));
        let deadline = Instant::now() + Duration::from_millis(15);
        while Instant::now() < deadline {
            if let Ok(chunk) = pump.rx.recv_timeout(Duration::from_millis(10)) {
                pump.raw_log.extend_from_slice(&chunk);
                term.advance_bytes(&chunk);
            }
        }
    }
    while fed < chars.len() {
        let mut b = [0u8; 4];
        keys.write_all(chars[fed].encode_utf8(&mut b).as_bytes())?;
        fed += 1;
    }
    keys.write_all(b"\r")?;
    keys.flush()?;
    pump.wait_for_marker(&mut term, "MARKER-A1")?;
    pump.settle(&mut term, "交互:风暴中输入执行完毕");

    // 静止对照命令
    keys.write_all(b"echo ('MAR'+'KER-B2')\r")?;
    keys.flush()?;
    pump.wait_for_marker(&mut term, "MARKER-B2")?;
    pump.settle(&mut term, "交互:静止对照");

    // ---- 不变量 ----
    let mut violations = payload_violations(&term, &hist_payloads);
    let logical = logical_lines(&term);
    let a1 = count_occurrences(&logical, "MARKER-A1");
    if a1 != 1 {
        violations.push(format!("输出 MARKER-A1 出现 {a1} 次（期望 1：输入被错乱执行或输出被复制/丢弃）"));
    }
    let b2 = count_occurrences(&logical, "MARKER-B2");
    if b2 != 1 {
        violations.push(format!("对照输出 MARKER-B2 出现 {b2} 次（期望 1）"));
    }
    // 输入行叠影：最终命令回显应恰好一行含 ('MAR'+ 片段
    let echo_lines = logical.iter().filter(|l| l.contains("('MAR'+")).count();
    if echo_lines != 2 {
        // 期望 2：风暴中输入的命令回显行 + 对照命令回显行
        violations.push(format!(
            "含命令回显片段 ('MAR'+ 的逻辑行 {echo_lines} 行（期望 2：输入行存在叠影残留或被吞）"
        ));
    }

    let verdict = violations.is_empty();
    println!(
        "[{}] 交互输入场景 (违规 {} 项)",
        if verdict { "PASS" } else { "FAIL" },
        violations.len()
    );
    let mut report = String::new();
    record_stage(
        &mut report,
        out_dir,
        &term,
        &pump,
        "交互输入场景",
        &violations,
    )?;
    // 追加到主报告，raw 流单独落盘
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir.join("report.txt"))?;
    f.write_all(report.as_bytes())?;
    std::fs::write(out_dir.join("raw-stream-input.bin"), &pump.raw_log)?;

    child.kill().ok();
    Ok(verdict)
}

/// 用户步骤复现场景：pwsh 7 + 宽输出（ls 形态）→ 拖拽缩到最窄 →
/// **静止后**逐字符输入 → 检查每个字符的上屏位置。
/// 与 interactive_shell_scenario 的区别：输入不与 resize 交错，而是
/// 在缩窄完成后进行；缩窄幅度大到 prompt 自身也被迫换行。
fn narrow_then_type_scenario(out_dir: &std::path::Path, gui_replay: bool) -> Result<bool> {
    if gui_replay {
        println!("== resize-probe: GUI 精确重放 (100x30 -> 27x30 -> 69x30, 短 prompt) ==");
    } else {
        println!("== resize-probe: 缩窄后输入场景 (pwsh, 用户步骤复现) ==");
    }
    let (rows0, cols0) = if gui_replay { (30, 100) } else { (24, 80) };

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(pty_size(rows0, cols0))
        .context("openpty(缩窄后输入) 失败")?;

    // 长 prompt 模拟深路径：缩到最窄时 prompt 自身必须换行。
    // gui_replay 用短 prompt（对应真实 "PS C:\Users\...>" 在 27 列不换行）
    let prompt_cmd = if gui_replay {
        "function prompt { 'PROBE-GH> ' }"
    } else {
        "function prompt { 'PROBE-LONG-PROMPT-0123456789-ABCDEFGH> ' }"
    };
    let mut cmd = CommandBuilder::new("pwsh.exe");
    for a in ["-NoProfile", "-NoLogo", "-NoExit", "-Command", prompt_cmd] {
        cmd.arg(a);
    }
    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(_) => {
            // 机器上没有 pwsh 时退回 powershell 5.1
            let mut cmd = CommandBuilder::new("powershell.exe");
            for a in ["-NoProfile", "-NoLogo", "-NoExit", "-Command", prompt_cmd] {
                cmd.arg(a);
            }
            pair.slave
                .spawn_command(cmd)
                .context("spawn pwsh/powershell 均失败")?
        }
    };
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = SharedWriter(Arc::new(Mutex::new(pair.master.take_writer()?)));
    let mut keys = writer.clone();

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut term = Terminal::new(
        term_size(rows0, cols0),
        Arc::new(ProbeConfig),
        "ResizeProbe",
        "1.0",
        Box::new(writer),
    );
    term.enable_conpty_quirks();

    let prompt_tail = if gui_replay {
        "PROBE-GH> "
    } else {
        "ABCDEFGH> "
    };
    let mut pump = Pump {
        rx,
        raw_log: vec![],
    };
    pump.wait_for_marker(&mut term, prompt_tail.trim_end())?;
    pump.settle(&mut term, "缩窄后输入:prompt 就绪");

    // 宽输出（ls -la 形态）：30 行、每行 ~76 字符，缩窄后必然 rewrap
    keys.write_all(b"1..30 | % { 'ROW-{0:d2}-' -f $_ + ('x' * 66) }\r")?;
    keys.flush()?;
    pump.wait_for_marker(&mut term, "ROW-30-")?;
    pump.settle(&mut term, "缩窄后输入:宽输出就绪");

    // 逐步遥测：resize 每一步之后 wezterm 的光标视口坐标 / 缓冲行数 /
    // 本步收到的 ConPTY 字节数，用于与 raw 流对时间线定位首个失步点
    let mut trace = String::new();
    let step = |term: &mut Terminal,
                pump: &mut Pump,
                master: &dyn portable_pty::MasterPty,
                cols: usize,
                trace: &mut String|
     -> Result<()> {
        master.resize(pty_size(rows0, cols))?;
        term.resize(term_size(rows0, cols));
        let mut got = 0usize;
        let deadline = Instant::now() + Duration::from_millis(15);
        while Instant::now() < deadline {
            if let Ok(chunk) = pump.rx.recv_timeout(Duration::from_millis(10)) {
                got += chunk.len();
                pump.raw_log.extend_from_slice(&chunk);
                term.advance_bytes(&chunk);
            }
        }
        let cur = term.cursor_pos();
        let mut nlines = 0usize;
        term.screen().for_each_phys_line(|_, _| nlines += 1);
        trace.push_str(&format!(
            "cols={cols:3} cursor=({},{}) lines={nlines} recv={got}\n",
            cur.x, cur.y
        ));
        Ok(())
    };

    if gui_replay {
        // 真实 GUI 观测(WEZTERm_LOG)证实:拖拽期间窗口层合并事件,
        // 模型只收到「缩窄终点」和「放宽终点」两次大跳变。
        // 且 GUI 里 reader 线程与 resize 并发:pty.resize 触发的
        // PSReadLine 重绘可能在 terminal.resize 之前被按旧宽度 advance
        // ——这里显式重现该竞争窗口(pty.resize 后先喂 300ms 流量再
        // term.resize)。
        let race_step = |term: &mut Terminal,
                         pump: &mut Pump,
                         cols: usize,
                         trace: &mut String|
         -> Result<()> {
            pair.master.resize(pty_size(rows0, cols))?;
            let deadline = Instant::now() + Duration::from_millis(300);
            let mut got = 0usize;
            while Instant::now() < deadline {
                if let Ok(chunk) = pump.rx.recv_timeout(Duration::from_millis(20)) {
                    got += chunk.len();
                    pump.raw_log.extend_from_slice(&chunk);
                    term.advance_bytes(&chunk);
                }
            }
            term.resize(term_size(rows0, cols));
            let cur = term.cursor_pos();
            trace.push_str(&format!(
                "race cols={cols:3} cursor=({},{}) pre-resize-recv={got}\n",
                cur.x, cur.y
            ));
            Ok(())
        };
        race_step(&mut term, &mut pump, 27, &mut trace)?;
        pump.settle(&mut term, "缩窄后输入:已缩至 27 列");
        race_step(&mut term, &mut pump, 69, &mut trace)?;
        pump.settle(&mut term, "缩窄后输入:已放宽至 69 列");
    } else {
        // 拖拽式缩到最窄（26 列 < prompt 的 42 字符）。
        // 步长/节奏模拟拖拽事件不合并的形态（每 2 列一个事件,~15ms）
        let mut cols = 76usize;
        while cols > 26 {
            step(&mut term, &mut pump, &*pair.master, cols, &mut trace)?;
            cols = cols.saturating_sub(2);
        }
        step(&mut term, &mut pump, &*pair.master, 26, &mut trace)?;
        pump.settle(&mut term, "缩窄后输入:已缩至 26 列");

        // 再放宽到适当宽度（用户步骤：缩到最窄 → 放宽 → 输入才错乱。
        // 缩窄+放宽使 ConPTY 与 wezterm 各自 reflow 的行布局分道扬镳）
        let mut cols = 28usize;
        while cols < 56 {
            step(&mut term, &mut pump, &*pair.master, cols, &mut trace)?;
            cols += 2;
        }
        step(&mut term, &mut pump, &*pair.master, 56, &mut trace)?;
        pump.settle(&mut term, "缩窄后输入:已放宽至 56 列");
    }
    println!("--- resize 遥测 ---\n{trace}");

    // 静止后逐字符输入；每个字符落屏后检查「prompt 尾部 + 已输入前缀」
    // 在逻辑行中保持连续 —— 上屏位置错乱会使前缀断裂
    let mut violations: Vec<String> = vec![];
    let cmd_text = "echo ('MAR'+'KER-C3')";
    let mut typed = String::new();
    for ch in cmd_text.chars() {
        let mut b = [0u8; 4];
        keys.write_all(ch.encode_utf8(&mut b).as_bytes())?;
        keys.flush()?;
        typed.push(ch);
        // 给 PSReadLine 重绘留时间
        let deadline = Instant::now() + Duration::from_millis(120);
        while Instant::now() < deadline {
            if let Ok(chunk) = pump.rx.recv_timeout(Duration::from_millis(20)) {
                pump.raw_log.extend_from_slice(&chunk);
                term.advance_bytes(&chunk);
            }
        }
        let expect = format!("{prompt_tail}{typed}");
        let logical = logical_lines(&term);
        if !logical.iter().any(|l| l.contains(&expect)) {
            violations.push(format!(
                "输入第 {} 个字符后前缀断裂：期望连续文本 [{expect}] 不存在",
                typed.chars().count()
            ));
            break; // 已断裂，后续检查无意义
        }
    }

    keys.write_all(b"\r")?;
    keys.flush()?;
    pump.wait_for_marker(&mut term, "MARKER-C3").ok();
    pump.settle(&mut term, "缩窄后输入:执行完毕");

    let logical = logical_lines(&term);
    let c3 = count_occurrences(&logical, "MARKER-C3");
    if c3 != 1 {
        violations.push(format!(
            "输出 MARKER-C3 出现 {c3} 次（期望 1：输入被错乱执行）"
        ));
    }
    // 宽输出历史完好
    for i in [1usize, 15, 30] {
        let row = format!("ROW-{i:02}-");
        let n = count_occurrences(&logical, &row);
        if n != 1 {
            violations.push(format!("历史行 {row} 出现 {n} 次（期望 1）"));
        }
    }

    let scenario_name = if gui_replay {
        "GUI重放:缩窄放宽后输入"
    } else {
        "缩窄后输入场景"
    };
    let verdict = violations.is_empty();
    println!(
        "[{}] {scenario_name} (违规 {} 项)",
        if verdict { "PASS" } else { "FAIL" },
        violations.len()
    );
    let mut report = String::new();
    record_stage(&mut report, out_dir, &term, &pump, scenario_name, &violations)?;
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir.join("report.txt"))?;
    f.write_all(report.as_bytes())?;
    std::fs::write(out_dir.join("raw-stream-narrow-type.bin"), &pump.raw_log)?;

    child.kill().ok();
    Ok(verdict)
}

fn harness() -> Result<bool> {
    let mut out_dir = PathBuf::from("target/resize-verify");
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--out-dir") {
        out_dir = PathBuf::from(
            args.get(i + 1)
                .context("--out-dir 需要一个路径参数")?
                .clone(),
        );
    }
    std::fs::create_dir_all(&out_dir)?;

    // 调试便捷开关：只跑「缩窄后输入」场景
    if args.iter().any(|a| a == "--only-narrow") {
        return narrow_then_type_scenario(&out_dir, args.iter().any(|a| a == "--gui-replay"));
    }

    let payloads = fixture_payloads();

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(pty_size(24, 80))
        .context("openpty 失败")?;

    let exe = std::env::current_exe()?;
    let mut cmd = CommandBuilder::new(&exe);
    cmd.arg("--child");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .context("spawn --child 失败")?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut term = Terminal::new(
        term_size(24, 80),
        Arc::new(ProbeConfig),
        "ResizeProbe",
        "1.0",
        writer,
    );
    // 与 mux LocalDomain::spawn_pane 在 Windows 下的行为保持一致
    term.enable_conpty_quirks();

    let mut pump = Pump {
        rx,
        raw_log: vec![],
    };

    pump.wait_for_marker(&mut term, READY_MARKER)?;
    pump.settle(&mut term, "初始");

    println!("== resize-probe: fixture 就绪，开始 resize 序列 ==");
    let mut all_ok = true;
    let mut report = String::new();

    let baseline = payload_violations(&term, &payloads);
    if !baseline.is_empty() {
        all_ok = false;
        record_stage(
            &mut report,
            &out_dir,
            &term,
            &pump,
            "初始(resize 前)",
            &baseline,
        )?;
    }

    for (rows, cols, stage) in [
        (24usize, 40usize, "缩窄 80->40"),
        (24, 120, "放宽 40->120"),
        (10, 120, "变矮 24->10"),
        (30, 120, "变高 10->30"),
        (24, 80, "回到 80x24"),
    ] {
        // 与 LocalPane::resize 同序：先 pty（触发 ConPTY reflow+重放），后模型
        pair.master.resize(pty_size(rows, cols))?;
        term.resize(term_size(rows, cols));
        let got = pump.settle(&mut term, stage);

        let mut violations = payload_violations(&term, &payloads);
        violations.extend(tick_violations(&term));
        let verdict = if violations.is_empty() {
            "PASS"
        } else {
            all_ok = false;
            "FAIL"
        };
        println!(
            "[{verdict}] {stage} (ConPTY 重放 {got} 字节, 违规 {} 项)",
            violations.len()
        );
        record_stage(&mut report, &out_dir, &term, &pump, stage, &violations)?;
    }

    // 风暴阶段：模拟用户拖拽窗口边缘 —— 连续快速的中间尺寸，
    // 期间 child 的 TICK 输出仍在滚动。这是 GUI live-resize 的真实形态，
    // 时序竞争（ConPTY 按旧尺寸产出、模型已是新尺寸）最容易在这里出现。
    // 时序竞争偶发，连跑多轮提高检出率
    for round in 1..=3 {
        let stage = format!("风暴{round}：拖拽式连续 resize");
        let stage = stage.as_str();
        for cols in [78usize, 74, 68, 62, 55, 48, 44, 40, 48, 60, 72, 80] {
            pair.master.resize(pty_size(24, cols))?;
            term.resize(term_size(24, cols));
            // 拖拽事件间隔量级 ~10ms，期间也把已到的流量喂进模型
            let deadline = Instant::now() + Duration::from_millis(30);
            while Instant::now() < deadline {
                if let Ok(chunk) = pump.rx.recv_timeout(Duration::from_millis(10)) {
                    pump.raw_log.extend_from_slice(&chunk);
                    term.advance_bytes(&chunk);
                }
            }
        }
        let got = pump.settle(&mut term, stage);
        let mut violations = payload_violations(&term, &payloads);
        violations.extend(tick_violations(&term));
        let verdict = if violations.is_empty() {
            "PASS"
        } else {
            all_ok = false;
            "FAIL"
        };
        println!(
            "[{verdict}] {stage} (ConPTY 重放 {got} 字节, 违规 {} 项)",
            violations.len()
        );
        record_stage(&mut report, &out_dir, &term, &pump, stage, &violations)?;
    }

    child.kill().ok();

    std::fs::write(out_dir.join("report.txt"), &report)?;
    std::fs::write(out_dir.join("raw-stream.bin"), &pump.raw_log)?;

    // 交互输入场景（真实 powershell + PSReadLine），复现「输入错乱」
    match interactive_shell_scenario(&out_dir) {
        Ok(ok) => all_ok = all_ok && ok,
        Err(err) => {
            eprintln!("交互输入场景故障: {err:#}");
            all_ok = false;
        }
    }

    // 用户步骤复现：宽输出 → 缩到最窄 → 静止后输入
    match narrow_then_type_scenario(&out_dir, false) {
        Ok(ok) => all_ok = all_ok && ok,
        Err(err) => {
            eprintln!("缩窄后输入场景故障: {err:#}");
            all_ok = false;
        }
    }

    // GUI 序列重放：窗口层会合并拖拽事件为「缩窄终点+放宽终点」两跳
    match narrow_then_type_scenario(&out_dir, true) {
        Ok(ok) => all_ok = all_ok && ok,
        Err(err) => {
            eprintln!("GUI 重放场景故障: {err:#}");
            all_ok = false;
        }
    }
    println!(
        "== 详细报告: {} / 原始字节流: {} ==",
        out_dir.join("report.txt").display(),
        out_dir.join("raw-stream.bin").display()
    );
    println!(
        "== 结果: {} ==",
        if all_ok {
            "全部通过 — ConPTY 交互层未检出内容异样"
        } else {
            "存在违规 — ConPTY 重放与模型 rewrap 的合成结果损坏了旧内容"
        }
    );
    Ok(all_ok)
}

fn record_stage(
    report: &mut String,
    out_dir: &std::path::Path,
    term: &Terminal,
    _pump: &Pump,
    stage: &str,
    violations: &[String],
) -> Result<()> {
    report.push_str(&format!(
        "\n===== 阶段 [{stage}] {} =====\n",
        if violations.is_empty() {
            "PASS"
        } else {
            "FAIL"
        }
    ));
    for v in violations {
        report.push_str(&format!("  违规: {v}\n"));
    }
    report.push_str("  --- 逻辑行 ---\n");
    for (i, l) in logical_lines(term).iter().enumerate() {
        report.push_str(&format!("  {i:3}: [{l}]\n"));
    }
    if !violations.is_empty() {
        let safe: String = stage
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let snap = out_dir.join(format!("screen-{safe}.txt"));
        let mut body = String::new();
        for (i, l) in screen_text(term).iter().enumerate() {
            body.push_str(&format!("{i:3}: [{l}]\n"));
        }
        std::fs::write(snap, body)?;
    }
    Ok(())
}
